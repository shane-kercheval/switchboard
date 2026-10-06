<script lang="ts">
  import WelcomeScreen from "$lib/components/WelcomeScreen.svelte";

  let { width }: { width: number } = $props();
</script>

<div data-testid="welcome-screen-host" style:width={`${width}px`}>
  <WelcomeScreen onAddProject={() => {}} />
</div>
