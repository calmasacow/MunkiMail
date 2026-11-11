import { invoke } from "@tauri-apps/api/core";
import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { BUNDLED_THEMES } from "./themes/omarchy";

export type Palette = { name: string; colors: Record<string, string> };
export type ThemeOption = {
  id: string;
  label: string;
  description: string;
  swatch: string[];
};
export const THEME_STORAGE_KEY = "munkimail-theme";
const required = [
  "accent",
  "background",
  "foreground",
  "bright_foreground",
  "red",
  "yellow",
  "green",
];
const hex = /^#[\da-f]{6}$/i;
export function validPalette(value: unknown): value is Palette {
  if (!value || typeof value !== "object") return false;
  const p = value as Palette;
  return (
    typeof p.name === "string" &&
    !!p.colors &&
    required.every((k) => hex.test(p.colors[k])) &&
    Object.values(p.colors).every((v) => typeof v === "string" && hex.test(v))
  );
}
export function luminance(color: string) {
  return [1, 3, 5].reduce((sum, start, i) => {
    const v = parseInt(color.slice(start, start + 2), 16) / 255;
    return (
      sum +
      (v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4) *
        [0.2126, 0.7152, 0.0722][i]
    );
  }, 0);
}
function contrast(a: string, b: string) {
  const x = luminance(a),
    y = luminance(b);
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
}
export function resolveTheme(id: string | undefined, omarchy: Palette | null) {
  if (!id || id === "auto") return omarchy ? "omarchy" : "original";
  if (id === "dark") return "original";
  if (id === "light") return "flexoki-light";
  if (id === "omarchy") return omarchy ? id : "original";
  return id === "original" || BUNDLED_THEMES.some((t) => t.id === id)
    ? id
    : "original";
}
export function cachedTheme(): string | undefined {
  try {
    return localStorage.getItem(THEME_STORAGE_KEY) || undefined;
  } catch {
    return undefined;
  }
}
export function rememberTheme(id: string) {
  try {
    localStorage.setItem(THEME_STORAGE_KEY, id);
  } catch {
    /* OS settings still persist the choice. */
  }
}
export async function discoverOmarchy(): Promise<Palette | null> {
  if (
    !(window as unknown as { __TAURI_INTERNALS__?: unknown })
      .__TAURI_INTERNALS__
  )
    return null;
  const value = await invoke<unknown>("get_omarchy_theme").catch(() => null);
  return validPalette(value) ? value : null;
}
export function themeOptions(omarchy: Palette | null): ThemeOption[] {
  const options: ThemeOption[] = [
    {
      id: "original",
      label: "Original",
      description: "MunkiMail's warm ink palette with coral accents.",
      swatch: ["#191917", "#ee9b85", "#e8e5df"],
    },
    ...BUNDLED_THEMES.map((t) => ({
      id: t.id,
      label: t.label,
      description: `${t.origin === "stock" ? "Stock" : "Custom"} Omarchy palette · ${t.mode}.`,
      swatch: [
        t.colors.background,
        t.colors.accent,
        t.colors.bright_foreground,
      ],
    })),
  ];
  if (omarchy)
    options.unshift({
      id: "omarchy",
      label: "Follow Omarchy",
      description: `Tracks your current Omarchy desktop palette: ${omarchy.name}.`,
      swatch: [
        omarchy.colors.background,
        omarchy.colors.accent,
        omarchy.colors.bright_foreground,
      ],
    });
  return options;
}
function palette(
  id: string,
  omarchy: Palette | null,
): Record<string, string> | undefined {
  return id === "omarchy"
    ? omarchy?.colors
    : BUNDLED_THEMES.find((t) => t.id === id)?.colors;
}
export function paletteVariables(colors: Record<string, string>) {
  const c = colors;
  // Include black/white when a supplied palette's own foreground has poor contrast.
  const onAccent = [
    c.background,
    c.bright_foreground,
    "#000000",
    "#ffffff",
  ].sort((a, b) => contrast(b, c.accent) - contrast(a, c.accent))[0];
  const hover = c.selection ?? c.lighter_background ?? c.background;
  const accentHover = c.bright_blue ?? c.accent;
  const readable = (background: string) =>
    [c.bright_foreground, c.background, "#000000", "#ffffff"].sort(
      (a, b) => contrast(b, background) - contrast(a, background),
    )[0];
  return {
    "--on-hover":
      contrast(c.bright_foreground, hover) >= 4.5
        ? c.bright_foreground
        : readable(hover),
    "--on-accent-hover": readable(accentHover),
    "--bg": c.background,
    "--side": c.dark_background ?? c.background,
    "--panel": c.background,
    "--raised": c.lighter_background ?? c.background,
    "--line": c.muted ?? c.dark_foreground ?? c.foreground,
    "--text": c.bright_foreground,
    "--muted": c.light_foreground ?? c.foreground,
    "--accent": c.accent,
    "--accent-hover": c.bright_blue ?? c.accent,
    "--accent-bg": `color-mix(in srgb, ${c.accent} 12%, transparent)`,
    "--on-accent": onAccent,
    "--input": c.lighter_background ?? c.background,
    "--hover": hover,
    "--danger": c.red,
    "--success": c.green,
    "--warning": c.yellow,
  };
}
const properties = Object.keys(
  paletteVariables(Object.fromEntries(required.map((k) => [k, "#000000"]))),
);
export function applyTheme(id: string | undefined, omarchy: Palette | null) {
  const root = document.documentElement;
  const resolved = resolveTheme(id, omarchy);
  for (const key of properties) root.style.removeProperty(key);
  root.style.removeProperty("color-scheme");
  root.dataset.theme = resolved;
  root.toggleAttribute("data-omarchy", omarchy !== null);
  const c = palette(resolved, omarchy);
  if (c) {
    for (const [key, value] of Object.entries(paletteVariables(c)))
      root.style.setProperty(key, value);
    root.style.colorScheme = luminance(c.background) > 0.5 ? "light" : "dark";
  }
}
export function readerColors(id: string | undefined, omarchy: Palette | null) {
  const resolved = resolveTheme(id, omarchy),
    c = palette(resolved, omarchy);
  if (c)
    return {
      text: c.bright_foreground,
      background: c.background,
      accent: c.accent,
      mode: luminance(c.background) > 0.5 ? "light" : "dark",
    };
  return {
    text: "#e8e5df",
    background: "#191917",
    accent: "#ee9b85",
    mode: "dark",
  };
}
export function ThemePicker(props: {
  value: string;
  omarchy: Palette | null;
  onChange: (id: string) => void;
}) {
  const [open, setOpen] = createSignal(false);
  let container!: HTMLDivElement;
  let trigger!: HTMLButtonElement;
  const options = () => themeOptions(props.omarchy);
  const selected = () =>
    options().find((t) => t.id === props.value) || options()[0];
  const swatch = (colors: string[]) => (
    <span class="theme-swatch" aria-hidden="true">
      <For each={colors}>{(c) => <i style={{ background: c }} />}</For>
    </span>
  );
  const dismiss = (e: MouseEvent) => {
    if (!container.contains(e.target as Node)) setOpen(false);
  };
  onMount(() => document.addEventListener("click", dismiss));
  onCleanup(() => document.removeEventListener("click", dismiss));
  return (
    <div
      class="theme-choice"
      ref={container}
      onKeyDown={(e) => {
        if (e.key === "Escape" && open()) {
          e.preventDefault();
          e.stopPropagation();
          setOpen(false);
          trigger.focus();
        }
        if (open() && ["ArrowDown", "ArrowUp", "Home", "End"].includes(e.key)) {
          e.preventDefault();
          const items = [
            ...container.querySelectorAll<HTMLButtonElement>(
              '[role="menuitemradio"]',
            ),
          ];
          const index = items.indexOf(
            document.activeElement as HTMLButtonElement,
          );
          items[
            e.key === "Home"
              ? 0
              : e.key === "End"
                ? items.length - 1
                : (index + (e.key === "ArrowUp" ? -1 : 1) + items.length) %
                  items.length
          ]?.focus();
        }
      }}
    >
      <span>Appearance</span>
      <button
        ref={trigger}
        type="button"
        class="theme-select"
        aria-label={`Appearance: ${selected().label}`}
        aria-haspopup="menu"
        aria-expanded={open()}
        onClick={() => {
          setOpen((v) => !v);
          if (open())
            queueMicrotask(() =>
              container
                .querySelector<HTMLButtonElement>('[aria-checked="true"]')
                ?.focus(),
            );
        }}
      >
        {swatch(selected().swatch)}
        <span>{selected().label}</span>
      </button>
      <Show when={open()}>
        <div class="theme-menu" role="menu" aria-label="Appearance themes">
          <For each={options()}>
            {(option) => (
              <button
                type="button"
                role="menuitemradio"
                aria-checked={props.value === option.id}
                onClick={() => {
                  props.onChange(option.id);
                  setOpen(false);
                  trigger.focus();
                }}
              >
                {swatch(option.swatch)}
                <span>{option.label}</span>
              </button>
            )}
          </For>
        </div>
      </Show>
      <small>{selected().description}</small>
    </div>
  );
}
