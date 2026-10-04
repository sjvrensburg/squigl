<script lang="ts">
  // What to do when the picture is not coming: the engine says which problem it
  // is (squigl_engine::stream::Problem), this says what a person can do about it.
  import { dispatch, type Problem } from "./lib/engine";
  import { guidance } from "./lib/guidance";

  let { problem, reason, onopen }: {
    problem: Problem;
    reason: string;
    onopen: () => void;
  } = $props();

  let heading = $state<HTMLHeadingElement>();
  let hidden = $state(false);
  const platform = navigator.userAgent.includes("Windows")
    ? "windows"
    : navigator.userAgent.includes("Mac")
      ? "mac"
      : "linux";
  const help = $derived(guidance(problem, platform));

  // A new problem is shown again (even if the last was hidden) and takes focus,
  // for an OS magnifier that follows it.
  $effect(() => {
    void problem;
    hidden = false;
    queueMicrotask(() => heading?.focus());
  });
</script>

{#if !hidden}
  <section class="connection" aria-labelledby="connection-title">
    <h2 id="connection-title" tabindex="-1" bind:this={heading}>{help.title}</h2>
    <ol>
      {#each help.steps as step}
        <li>{step}</li>
      {/each}
    </ol>
    {#if problem === "other"}
      <p class="detail">{reason}</p>
    {/if}
    <p>Squigl keeps trying by itself.</p>
    <div class="actions">
      <button onclick={() => dispatch({ type: "reconnect" })}>Try now</button>
      <button onclick={onopen}>Open an image instead</button>
      <button onclick={() => (hidden = true)}>Hide</button>
    </div>
  </section>
{/if}

<style>
  .connection {
    position: absolute;
    inset: 1rem;
    margin: auto;
    max-width: 40rem;
    height: fit-content;
    max-height: calc(100% - 2rem);
    overflow: auto;
    padding: 1.25rem 1.5rem;
    background: var(--panel);
    color: var(--text);
    border: 0.15rem solid var(--edge);
    border-radius: 0.5rem;
  }
  h2 {
    margin-top: 0;
  }
  li {
    margin: 0.4rem 0;
  }
  .detail {
    font-family: ui-monospace, monospace;
    font-size: 0.9em;
    white-space: pre-wrap;
    opacity: 0.9;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 0.75rem;
  }
</style>
