import { mount } from "svelte";
import App from "./App.svelte";
import { initTheme } from "$lib/theme.svelte";
import "./app.css";

initTheme();

const target = document.getElementById("app");
if (!target) {
  throw new Error("missing #app mount node");
}

mount(App, { target });
