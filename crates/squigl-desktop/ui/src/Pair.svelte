<script lang="ts">
  // Pairing a phone's browser: each address this computer has (the LAN's, and any
  // overlay network's: Tailscale, ZeroTier, Nebula) with its QR code, and what the
  // phone will ask. The server runs only while this is open (pairing_start/stop),
  // and a phone that pairs closes it: the picture is what it was opened for.
  import { invoke } from "@tauri-apps/api/core";
  import type { StreamSlice } from "./lib/engine";

  let { open = $bindable(), stream, tailscaleHttps }: {
    open: boolean;
    stream: StreamSlice | null;
    tailscaleHttps: boolean;
  } = $props();

  interface Offer {
    network: string;
    overlay: boolean;
    url: string;
    qr_svg: string;
    trusted: boolean;
  }

  let dialog: HTMLDialogElement;
  let heading: HTMLHeadingElement;
  let offers = $state<Offer[] | null>(null);
  let chosen = $state(0);
  const offer = $derived(offers?.[chosen] ?? null);
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
    offers = null;
    chosen = 0;
    error = "";
    copied = false;
    awaiting = false;
    try {
      offers = await invoke<Offer[]>("pairing_start", { tailscaleHttps });
    } catch (e) {
      error = String(e);
    }
  }

  async function copy() {
    if (!offer) return;
    try {
      await navigator.clipboard.writeText(offer.url);
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
  {:else if offers && offer}
    {#if offers.length > 1}
      <fieldset class="networks">
        <legend>The phone is on</legend>
        {#each offers as o, i}
          <label>
            <input
              type="radio"
              name="network"
              checked={i === chosen}
              onchange={() => {
                chosen = i;
                copied = false;
              }}
            />
            {o.network}
          </label>
        {/each}
      </fieldset>
    {/if}
    <div class="pair">
      <div class="qr" role="img" aria-label="QR code of the address below">
        {@html offer.qr_svg}
      </div>
      <ol>
        {#if offer.overlay}
          <li>Open {offer.network} on the phone, on the same network as this computer.</li>
        {:else}
          <li>Connect the phone to the same Wi-Fi as this computer.</li>
        {/if}
        <li>Point the phone's camera at the code, or type the address below into its browser.</li>
        {#if !offer.trusted}
          <li>
            The browser warns that the connection is not private: choose Advanced, then
            Proceed (or Accept the risk). Each phone asks once.
          </li>
        {/if}
        <li>Tap Start streaming, and allow the camera.</li>
      </ol>
    </div>
    <p class="address">
      <span>Address</span>
      <code>{offer.url}</code>
      <button onclick={copy}>{copied ? "Copied" : "Copy"}</button>
    </p>
    <p role="status">Waiting for a phone…</p>
    <p class="hint">
      Phone on a different network, such as mobile data? Connect both to the same
      hotspot, or install Tailscale on both and pair through its address.
    </p>
    <p class="hint">
      Phone on the same network but the page will not open, or never starts streaming?
      This computer's firewall may be in the way: allow Squigl when it asks (on Windows,
      for the kind of network this is: Private for home).
    </p>
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
  .networks {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem 1.25rem;
    margin: 0 0 1rem;
    border: 0.1rem solid var(--edge);
    border-radius: 0.35rem;
  }
  .networks label {
    display: flex;
    gap: 0.4rem;
    align-items: center;
  }
  .hint {
    max-width: 40rem;
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
