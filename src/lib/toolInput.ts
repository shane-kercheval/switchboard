const PREVIEW_KEYS = [
  "description",
  "toolSummary",
  "toolAction",
  "command",
  "cmd",
  "CommandLine",
  "file_path",
  "path",
  "uri",
  "url",
  "query",
  "pattern",
  "prompt",
  "text",
  "name",
] as const;

const SENSITIVE_KEY =
  /(?:^|[_-])(token|secret|password|passwd|authorization|auth[_-]?token|api[_-]?key|credential|cookie|session[_-]?(token|cookie)|bearer)(?:$|[_-])/i;
const AUTH_BEARER_PATTERN = /(Authorization\s*:\s*Bearer\s+)([^\s"'\\]+)/gi;
const COOKIE_HEADER_PATTERN = /(Cookie\s*:\s*)([^"'\r\n]+)/gi;
const SECRET_ASSIGNMENT_PATTERN =
  /((?:api[_-]?key|access[_-]?token|auth[_-]?token|token)=)([^&\s"'\\]+)/gi;
const API_KEY_PATTERN = /\bsk[-_][A-Za-z0-9_-]{8,}\b/g;
const JSON_STRING_VALUE_KEYS = new Set(["toolSummary", "toolAction", "CommandLine", "Cwd"]);

type JsonRecord = Record<string, unknown>;

export function toolInputPreview(input: unknown): string | undefined {
  const value = redacted(input);
  if (value === undefined || value === null) return undefined;
  if (typeof value === "string") return oneLine(value);
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  if (Array.isArray(value)) return previewArray(value);
  if (isRecord(value)) {
    for (const key of PREVIEW_KEYS) {
      if (!(key in value)) continue;
      const preview = previewValue(value[key]);
      if (preview !== undefined) return preview;
    }
    return compactJson(value);
  }
  return undefined;
}

export function formatToolInput(input: unknown): string | undefined {
  const value = redacted(input);
  if (value === undefined || value === null) return undefined;
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  return JSON.stringify(value, null, 2);
}

function previewValue(value: unknown): string | undefined {
  if (value === undefined || value === null) return undefined;
  if (typeof value === "string") return oneLine(value);
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  if (Array.isArray(value)) return previewArray(value);
  if (isRecord(value)) return compactJson(value);
  return undefined;
}

function previewArray(value: unknown[]): string | undefined {
  if (value.length === 0) return "[]";
  if (value.every((item) => typeof item === "string")) return oneLine(value.join(" "));
  return compactJson(value);
}

function oneLine(value: string): string | undefined {
  const trimmed = value.replace(/\s+/g, " ").trim();
  return trimmed === "" ? undefined : trimmed;
}

function compactJson(value: unknown): string | undefined {
  const json = JSON.stringify(value);
  return json === undefined ? undefined : oneLine(json);
}

function redacted(value: unknown, key?: string): unknown {
  if (Array.isArray(value)) return value.map((child) => redacted(child));
  if (typeof value === "string") {
    const displayValue =
      key !== undefined && JSON_STRING_VALUE_KEYS.has(key) ? unwrapJsonString(value) : value;
    return redactString(displayValue);
  }
  if (!isRecord(value)) return value;
  return Object.fromEntries(
    Object.entries(value).map(([key, child]) => [
      key,
      isSensitiveKey(key) ? "[redacted]" : redacted(child, key),
    ]),
  );
}

function unwrapJsonString(value: string): string {
  const trimmed = value.trim();
  if (!trimmed.startsWith('"') || !trimmed.endsWith('"')) return value;
  try {
    const parsed: unknown = JSON.parse(trimmed);
    return typeof parsed === "string" ? parsed : value;
  } catch {
    return value;
  }
}

// Best-effort display redaction only; tool inputs can still contain secrets in
// formats we do not recognize.
function redactString(value: string): string {
  return value
    .replace(AUTH_BEARER_PATTERN, "$1[redacted]")
    .replace(COOKIE_HEADER_PATTERN, "$1[redacted]")
    .replace(SECRET_ASSIGNMENT_PATTERN, "$1[redacted]")
    .replace(API_KEY_PATTERN, "[redacted]");
}

function isSensitiveKey(key: string): boolean {
  const separated = key.replace(/([a-z0-9])([A-Z])/g, "$1_$2");
  return SENSITIVE_KEY.test(separated);
}

function isRecord(value: unknown): value is JsonRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
