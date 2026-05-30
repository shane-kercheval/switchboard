/// <reference types="vite/client" />
/// <reference types="svelte" />

interface ImportMetaEnv {
  readonly VITE_DEV_PORT: string;
  readonly VITE_GIT_BRANCH: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
