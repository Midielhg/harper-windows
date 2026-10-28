<script lang="ts">
import { Button, Checkbox, CheckIcon, Select, SettingRow } from 'components';
import { onMount } from 'svelte';
import { Client } from '../../client';

/// One editable row. Kept as a list rather than an object so a half-typed abbreviation does not
/// collide with another row while the user is still typing it.
type TextShortcut = { abbreviation: string; expansion: string };

let textShortcuts: TextShortcut[] = [];
let isLoading = true;
let saveError = '';

onMount(() => {
	void loadTextShortcuts();
});

async function loadTextShortcuts() {
	try {
		const stored = await Client.getTextShortcuts();
		textShortcuts = Object.entries(stored).map(([abbreviation, expansion]) => ({
			abbreviation,
			expansion,
		}));
	} catch (error) {
		console.error('Unable to load text shortcuts.', error);
	} finally {
		isLoading = false;
	}
}

async function saveTextShortcuts() {
	const stored: Record<string, string> = {};

	for (const { abbreviation, expansion } of textShortcuts) {
		const trimmed = abbreviation.trim();

		// A row the user has not finished is not an error, it just has nothing to save yet.
		if (trimmed === '' || expansion.trim() === '') continue;

		stored[trimmed] = expansion.trim();
	}

	try {
		await Client.setTextShortcuts(stored);
		saveError = '';
	} catch (error) {
		console.error('Unable to save text shortcuts.', error);
		saveError = 'Could not save your shortcuts.';
	}
}

function addTextShortcut() {
	textShortcuts = [...textShortcuts, { abbreviation: '', expansion: '' }];
}

function removeTextShortcut(index: number) {
	textShortcuts = textShortcuts.filter((_, at) => at !== index);
	void saveTextShortcuts();
}

const shortcutItems = [
	{ id: 'show-menu', label: 'Show Harper menu', keys: ['Shift', 'Cmd', 'H'] },
	{ id: 'quick-review', label: 'Open quick review', keys: ['Ctrl', 'Cmd', 'Space'] },
	{ id: 'accept-last', label: 'Apply last suggestion', keys: ['Ctrl', 'E'] },
	{ id: 'dismiss-last', label: 'Dismiss last suggestion', keys: ['Ctrl', 'D'] },
	{ id: 'toggle-pause', label: 'Pause or resume checking', keys: ['Ctrl', 'Shift', 'P'] },
	{ id: 'add-word', label: 'Add word to dictionary', keys: ['Ctrl', 'Shift', 'A'] },
	{ id: 'next-suggestion', label: 'Jump to next suggestion', keys: [] },
];

let globalShortcuts = true;
let activationKey = 'off';
</script>

<section>
        <div class="stanza">
          <div class="eyebrow">Text Shortcuts</div>
          <p class="section-copy">
            Type the short form and Harper offers the long one, the same way it offers a spelling
            fix. For example <code>wo</code> for <code>without</code>, or <code>ok</code> for
            <code>okay</code>.
          </p>

          {#if isLoading}
            <p role="status">Loading shortcuts...</p>
          {:else}
            <div class="text-shortcut-list">
              {#each textShortcuts as shortcut, index (index)}
                <div class="text-shortcut-row">
                  <input
                    class="text-shortcut-input abbreviation"
                    type="text"
                    placeholder="wo"
                    aria-label="Short form"
                    bind:value={shortcut.abbreviation}
                    on:change={saveTextShortcuts}
                  />
                  <span class="text-shortcut-arrow" aria-hidden="true">&rarr;</span>
                  <input
                    class="text-shortcut-input expansion"
                    type="text"
                    placeholder="without"
                    aria-label="Replaced with"
                    bind:value={shortcut.expansion}
                    on:change={saveTextShortcuts}
                  />
                  <Button
                    unstyled
                    class="text-shortcut-remove"
                    type="button"
                    aria-label={`Remove the shortcut for ${shortcut.abbreviation || 'this row'}`}
                    on:click={() => removeTextShortcut(index)}
                  >
                    Remove
                  </Button>
                </div>
              {/each}
            </div>

            {#if textShortcuts.length === 0}
              <p class="section-copy">You have not added any shortcuts yet.</p>
            {/if}

            <Button unstyled class="text-shortcut-add" type="button" on:click={addTextShortcut}>
              Add a shortcut
            </Button>

            {#if saveError}
              <p class="text-shortcut-error" role="alert">{saveError}</p>
            {/if}
          {/if}
        </div>

        <div class="divider"></div>

        <div class="stanza">
          <div class="eyebrow">Keyboard Shortcuts</div>
          <p class="section-copy">
            Global shortcuts work from anywhere on macOS. Click a shortcut to preview recording.
          </p>

          <div class="shortcut-list">
            {#each shortcutItems as item}
              <Button unstyled class="shortcut-row" type="button" disabled title="Not wired yet">
                <span>{item.label}</span>
                {#if item.keys.length > 0}
                  <span class="kbd-group">
                    {#each item.keys as key}
                      <kbd>{key}</kbd>
                    {/each}
                  </span>
                {:else}
                  <em>Click to record...</em>
                {/if}
              </Button>
            {/each}
          </div>

          <SettingRow top>
            <strong>Allow shortcuts while other apps are focused</strong>
            <p>When off, Harper shortcuts only work while the Harper window is active.</p>
            <Checkbox
              slot="control"
              appearance="settings"
              checked={globalShortcuts}
              disabled
              title="Not wired yet"
            >
              {#if globalShortcuts}<CheckIcon className="control-icon" />{/if}
            </Checkbox>
          </SettingRow>
        </div>

        <div class="divider"></div>

        <div class="stanza">
          <div class="eyebrow">Activation</div>
          <SettingRow top>
            <strong>Activation key</strong>
            <p>Require a modifier key to enable Harper checking in a window.</p>
            <Select
              slot="control"
              unstyled
              class="select"
              disabled
              title="Not wired yet"
              bind:value={activationKey}
            >
              <option value="off">Off</option>
              <option value="option">Option</option>
              <option value="control">Control</option>
              <option value="shift">Shift</option>
            </Select>
          </SettingRow>
        </div>
      </section>
