<script lang="ts">
  // The settings dialog: colours and their adjustments, the view, the app's look,
  // and the keyboard. Sliders preview as they move and save when let go; every
  // other control saves at once.
  import { dispatch, type Config, type DisplayMode, type Theme } from "./lib/engine";
  import { MODES } from "./lib/modes";
  import { ACTIONS, keyFor, keyLabel, rebind, type Action } from "./lib/shortcuts";

  let { config, open = $bindable(), onnotice }: {
    config: Config;
    open: boolean;
    onnotice: (text: string) => void;
  } = $props();

  let dialog: HTMLDialogElement;
  let heading: HTMLHeadingElement;
  // The action waiting for its new key, if any.
  let capturing = $state<Action | null>(null);

  const THEMES: [Theme, string][] = [
    ["dark", "Dark"],
    ["light", "Light"],
    ["high-contrast-yellow", "High contrast, yellow on black"],
    ["high-contrast-white", "High contrast, white on black"],
  ];

  $effect(() => {
    if (open && !dialog.open) {
      dialog.showModal();
      // Focus where the dialog starts, for an OS magnifier following focus.
      heading.focus();
    } else if (!open && dialog.open) {
      dialog.close();
    }
  });

  async function save(next: Config) {
    try {
      await dispatch({ type: "set-config", config: next });
    } catch (e) {
      onnotice(`Could not save the settings: ${e}`);
    }
  }

  function preview(next: Config) {
    dispatch({ type: "preview-config", config: next }).catch(() => {});
  }

  function display<K extends keyof Config["display"]>(key: K, value: Config["display"][K]): Config {
    return { ...config, display: { ...config.display, [key]: value } };
  }

  function magnifier<K extends keyof Config["magnifier"]>(key: K, value: Config["magnifier"][K]): Config {
    return { ...config, magnifier: { ...config.magnifier, [key]: value } };
  }

  function desktop<K extends keyof Config["desktop"]>(key: K, value: Config["desktop"][K]): Config {
    return { ...config, desktop: { ...config.desktop, [key]: value } };
  }

  function ui<K extends keyof Config["ui"]>(key: K, value: Config["ui"][K]): Config {
    return { ...config, ui: { ...config.ui, [key]: value } };
  }

  const hex = (c: [number, number, number]) =>
    "#" + c.map((v) => v.toString(16).padStart(2, "0")).join("");
  const rgb = (h: string): [number, number, number] => [
    parseInt(h.slice(1, 3), 16),
    parseInt(h.slice(3, 5), 16),
    parseInt(h.slice(5, 7), 16),
  ];

  function captureKey(e: KeyboardEvent) {
    if (!capturing) return;
    e.preventDefault();
    e.stopPropagation();
    const action = capturing;
    capturing = null;
    if (e.key === "Escape") {
      onnotice("Shortcut unchanged");
      return;
    }
    if (["Shift", "Control", "Alt", "Meta", "Tab"].includes(e.key)) {
      capturing = action;
      return;
    }
    save(desktop("shortcuts", rebind(config.desktop, action, e.key)));
    const label = ACTIONS.find((a) => a.action === action)!.label;
    onnotice(`${label}: ${keyLabel(e.key)}`);
  }
</script>

<dialog
  bind:this={dialog}
  aria-labelledby="settings-title"
  onclose={() => {
    open = false;
    capturing = null;
  }}
  onkeydowncapture={captureKey}
>
  <h2 id="settings-title" tabindex="-1" bind:this={heading}>Settings</h2>

  <fieldset>
    <legend>Colours</legend>
    <label>
      Colours
      <select
        value={config.display.mode}
        onchange={(e) => save(display("mode", e.currentTarget.value as DisplayMode))}
      >
        {#each MODES as [mode, label]}
          <option value={mode}>{label}</option>
        {/each}
      </select>
    </label>
    {#if config.display.mode === "custom"}
      <label>
        Ink (writing)
        <input
          type="color"
          value={hex(config.display.ink)}
          onchange={(e) => save(display("ink", rgb(e.currentTarget.value)))}
        />
      </label>
      <label>
        Paper (background)
        <input
          type="color"
          value={hex(config.display.paper)}
          onchange={(e) => save(display("paper", rgb(e.currentTarget.value)))}
        />
      </label>
    {/if}
    <label>
      Contrast
      <input
        type="range"
        min="0.5"
        max="4"
        step="0.05"
        value={config.display.contrast}
        oninput={(e) => preview(display("contrast", Number(e.currentTarget.value)))}
        onchange={(e) => save(display("contrast", Number(e.currentTarget.value)))}
      />
      <output>{config.display.contrast.toFixed(2)}</output>
    </label>
    <label>
      Brightness
      <input
        type="range"
        min="-0.5"
        max="0.5"
        step="0.01"
        value={config.display.brightness}
        oninput={(e) => preview(display("brightness", Number(e.currentTarget.value)))}
        onchange={(e) => save(display("brightness", Number(e.currentTarget.value)))}
      />
      <output>{config.display.brightness.toFixed(2)}</output>
    </label>
    <label>
      Mid-tones
      <input
        type="range"
        min="0.3"
        max="3"
        step="0.05"
        value={config.display.gamma}
        oninput={(e) => preview(display("gamma", Number(e.currentTarget.value)))}
        onchange={(e) => save(display("gamma", Number(e.currentTarget.value)))}
      />
      <output>{config.display.gamma.toFixed(2)}</output>
    </label>
    <label>
      <input
        type="checkbox"
        checked={config.display.threshold !== null}
        disabled={config.display.mode === "normal" || config.display.mode === "inverted"}
        onchange={(e) => save(display("threshold", e.currentTarget.checked ? 0.5 : null))}
      />
      Only two colours (no greys)
    </label>
    {#if config.display.threshold !== null}
      <label>
        Cut-off
        <input
          type="range"
          min="0.05"
          max="0.95"
          step="0.01"
          value={config.display.threshold}
          oninput={(e) => preview(display("threshold", Number(e.currentTarget.value)))}
          onchange={(e) => save(display("threshold", Number(e.currentTarget.value)))}
        />
        <output>{config.display.threshold.toFixed(2)}</output>
      </label>
    {/if}
  </fieldset>

  <fieldset>
    <legend>View</legend>
    <label>
      <input
        type="checkbox"
        checked={config.magnifier.smooth}
        onchange={(e) => save(magnifier("smooth", e.currentTarget.checked))}
      />
      Smooth edges when magnified
    </label>
    <label>
      <input
        type="checkbox"
        checked={config.magnifier.reading_line}
        onchange={(e) => save(magnifier("reading_line", e.currentTarget.checked))}
      />
      Reading line across the middle
    </label>
    <label>
      Magnification at start
      <input
        type="number"
        min="1"
        max={config.magnifier.max_magnification}
        step="0.5"
        value={config.magnifier.magnification}
        onchange={(e) => save(magnifier("magnification", Number(e.currentTarget.value)))}
      />
    </label>
  </fieldset>

  <fieldset>
    <legend>Appearance</legend>
    <label>
      Theme
      <select
        value={config.desktop.theme}
        onchange={(e) => save(desktop("theme", e.currentTarget.value as Theme))}
      >
        {#each THEMES as [theme, label]}
          <option value={theme}>{label}</option>
        {/each}
      </select>
    </label>
  </fieldset>

  <fieldset>
    <legend>Readings</legend>
    <label>
      Reading text size
      <input
        type="range"
        min="10"
        max="60"
        step="1"
        value={config.ui.reading_size}
        aria-valuetext={`${config.ui.reading_size} points`}
        oninput={(e) => preview(ui("reading_size", Number(e.currentTarget.value)))}
        onchange={(e) => save(ui("reading_size", Number(e.currentTarget.value)))}
      />
      <output>{config.ui.reading_size} pt</output>
    </label>
  </fieldset>

  <fieldset>
    <legend>Phone pairing</legend>
    <label>
      <input
        type="checkbox"
        checked={config.desktop.tailscale_https}
        onchange={(e) => save(desktop("tailscale_https", e.currentTarget.checked))}
      />
      Over Tailscale, use Tailscale's certificate (no browser warning)
    </label>
    <p class="note">
      Needs HTTPS turned on for the tailnet. Getting the certificate publishes this
      computer's Tailscale name in public certificate logs.
    </p>
  </fieldset>

  <fieldset>
    <legend>Keyboard</legend>
    <label>
      <input
        type="checkbox"
        checked={config.desktop.single_key_shortcuts}
        onchange={(e) => save(desktop("single_key_shortcuts", e.currentTarget.checked))}
      />
      Single-key shortcuts (Space, R, M…)
    </label>
    <table>
      <thead>
        <tr><th scope="col">Action</th><th scope="col">Key</th><th scope="col"><span class="hidden">Change</span></th></tr>
      </thead>
      <tbody>
        {#each ACTIONS as { action, label }}
          {@const key = keyFor(action, config.desktop)}
          <tr>
            <th scope="row">{label}</th>
            <td>{key ? keyLabel(key) : "none"}</td>
            <td>
              <button onclick={() => (capturing = action)} aria-pressed={capturing === action}>
                {capturing === action ? "Press a key… (Esc: keep)" : "Change"}
              </button>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
    <button onclick={() => save(desktop("shortcuts", {}))}>Restore the default keys</button>
  </fieldset>

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
  fieldset {
    display: grid;
    gap: 0.6rem;
    margin: 0 0 1rem;
    border: 0.1rem solid var(--edge);
    border-radius: 0.35rem;
  }
  legend {
    font-weight: bold;
  }
  label {
    display: flex;
    gap: 0.6rem;
    align-items: center;
    flex-wrap: wrap;
  }
  input[type="range"] {
    flex: 1;
    min-width: 10rem;
  }
  table {
    border-collapse: collapse;
  }
  th,
  td {
    text-align: left;
    padding: 0.2rem 0.75rem 0.2rem 0;
  }
  .note {
    margin: 0;
    max-width: 38rem;
  }
  .hidden {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
  }
</style>
