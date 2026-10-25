<script lang="ts">
  import AppShell from "$lib/components/ui/AppShell.svelte";
  import SettingsSidebar from "$lib/components/SettingsSidebar.svelte";
  import SettingsView from "$lib/components/SettingsView.svelte";
  import type { SettingsSection } from "$lib/settingsSections";

  let { width }: { width: number } = $props();
  let section = $state<SettingsSection>("general");
</script>

<div class="flex h-[600px] flex-col" style:width={`${width}px`} data-testid="settings-host">
  <AppShell>
    {#snippet left()}
      <SettingsSidebar {section} onSelect={(value) => (section = value)} onClose={() => {}} />
    {/snippet}
    {#snippet center()}
      <div class="h-11 shrink-0">Settings</div>
      <SettingsView {section} onClose={() => {}} />
    {/snippet}
  </AppShell>
</div>
