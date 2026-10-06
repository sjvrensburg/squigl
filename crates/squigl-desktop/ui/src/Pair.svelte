<script lang="ts">
  // Pairing a phone's browser: the address and its QR code, and what the phone
  // will ask. The server runs only while this is open (pairing_start/stop), and a
  // phone that pairs closes it: the picture is what it was opened for.
  import { invoke } from "@tauri-apps/api/core";
  import type { StreamSlice } from "./lib/engine";

  let { open = $bindable(), stream }: {
    open: boolean;
    stream: StreamSlice | null;
  } = $props();

  let dialog: HTMLDialogElement;
  let heading: HTMLHeadingElement;
  let info = $state<{ url: string; qr_svg: string } | null>(null);
  let error = $state("");
  let copied = $state(false);
  // Whether, since opening, the paired-phone source has been without a picture: a
  // phone streaming after that is a new one.
  let awaiting = $state(false);

  $effect(() => {
    if (open && !dialog.open) {
      dialog.showModal();
      // Focus where the dialog starts, for an OS magnifier following focus.
      heading.focus();
      start();
    } else if (!open && dialog.open) {
      dialog.close();
    }
  });

  $effect(() => {
    if (!open || !stream) return;
    const network = stream.source.kind === "network";
    if (network && stream.status.state !== "streaming") awaiting = true;
    else if (network && awaiting) open = false;
  });

  async function start() {
    info = null;
    error = "";
    copied = false;
    awaiting = false;
    try {
      info = await invoke<{ url: string; qr_svg: string }>("pairing_start");
    } catch (e) {
      error = String(e);
    }
  }

  async function copy() {
    if (!info) return;
    try {
      await navigator.clipboard.writeText(info.url);
      copied = true;
    } catch {
      copied = false;
    }
  }
</script>

<dialog
  bind:this={dialog}
  aria-labelledby="pair-title"
  onclose={() => {
    open = false;
    invoke("pairing_stop").catch(() => {});
  }}
>
  <h2 id="pair-title" tabindex="-1" bind:this={heading}>Pair a phone</h2>

  {#if error}
    <p role="alert">Pairing could not start: {error}</p>
  {:else if info}
    <div class="pair">
      <div class="qr" role="img" aria-label="QR code of the address below">
        {@html info.qr_svg}
      </div>
      <ol>
        <li>Connect the phone to the same Wi-Fi as this computer.</li>
        <li>Point the phone's camera at the code, or type the address below into its browser.</li>
        <li>
          The browser warns that the connection is not private: choose Advanced, then
          Proceed (or Accept the risk). Each phone asks once.
        </li>
        <li>Tap Start streaming, and allow the camera.</li>
      </ol>
    </div>
    <p class="address">
      <span>Address</span>
      <code>{info.url}</code>
      <button onclick={copy}>{copied ? "Copied" : "Copy"}</button>
    </p>
    <p role="status">Waiting for a phone…</p>
  {:else}
    <p>Starting…</p>
  {/if}

  <form method="dialog">
    <button>Close</button>
  </form>
</dialog>

<style>
  dialog {
    max-width: min(46rem, 95vw);
    max-height: 90vh;
    background: var(--panel);
    color: var(--text);
    border: 0.15rem solid var(--edge);
    border-radius: 0.5rem;
    padding: 1rem 1.5rem;
  }
  dialog::backdrop {
    background: rgb(0 0 0 / 0.6);
  }
  h2 {
    margin-top: 0;
  }
  .pair {
    display: flex;
    flex-wrap: wrap;
    gap: 1rem 1.5rem;
    align-items: flex-start;
  }
  /* The code keeps its own black on white whatever the theme: a camera needs it. */
  .qr {
    flex: none;
    line-height: 0;
  }
  .qr :global(svg) {
    width: min(16rem, 70vw);
    height: auto;
  }
  ol {
    flex: 1;
    min-width: 14rem;
    margin: 0;
    padding-left: 1.5rem;
  }
  li {
    margin: 0 0 0.5rem;
  }
  .address {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
    align-items: center;
  }
  code {
    font-size: 1.25rem;
    word-break: break-all;
  }
</style>
