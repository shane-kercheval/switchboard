export function promptAuthoringPrompt(promptsDir: string | null): string {
  const destination = promptsDir
    ? `Save the file directly in this Switchboard local prompts folder:\n${promptsDir}`
    : "Before saving, ask me for the folder path shown in Switchboard Settings → Prompts → Local prompts. Do not guess the path.";

  return `Help me create a reusable local prompt for Switchboard.

First, ask what I want the prompt to accomplish and which inputs I want to fill in each time I use it. Keep it focused on one task.

${destination}

Create the folder if needed. Use a descriptive, hyphenated filename ending in .md, with a matching name in the metadata. Put the file directly in the folder, not a subfolder: Switchboard does not scan recursively. Check for an existing file or prompt with the same name before saving. Do not overwrite it unless I ask you to.

File format:
- Begin the file with YAML metadata between two lines containing ---. There must be no text before the opening ---.
- Include name and description as strings. The name identifies the prompt; the description appears in the picker.
- Omit arguments if none are needed. Otherwise, declare each argument with name, description, and an explicit required: true or required: false. Arguments default to optional when required is omitted.
- All argument values are strings. Use lowercase names with underscores for arguments.
- Write the prompt body after the closing ---. Use MiniJinja syntax: {{ target }} inserts an argument, and {% if focus %}...{% endif %} includes text only when an optional argument is filled in.
- Declare every user-supplied variable in arguments. Missing optional arguments render empty. Do not assume project paths, agent responses, or other workflow variables are automatically available. Do not use external templates or custom filters.

Minimal example with required and optional inputs (adapt this to my task):

\`\`\`markdown
---
name: explain-code
description: Explain a function or file in this project.
arguments:
  - name: target
    description: Function name or file path to explain.
    required: true
  - name: focus
    description: Optional aspect to focus on.
    required: false
---
Explain {{ target }}. Describe its purpose, inputs, outputs, and important edge cases.
{% if focus %}Focus on: {{ focus }}{% endif %}
\`\`\`

Before saving, check the YAML, metadata, argument names, and template syntax. Check that the prompt makes sense with optional arguments both filled in and omitted. If you can run Switchboard's renderer, validate it with sample values; otherwise, say that rendering has not been tested.

After saving, tell me the filename and prompt name. Explain that I should open the prompt picker in Switchboard's compose bar, choose “Sync prompts,” then select the new prompt, fill in its inputs, and review the rendered text before sending it to my selected agents. The prompt is available in every project on this Mac. A workflow can reference it as local:<name>.

If you cannot write to the folder, give me the proposed filename and complete Markdown file instead.`;
}
