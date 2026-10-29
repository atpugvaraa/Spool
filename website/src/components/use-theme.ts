"use client";

import { useSyncExternalStore } from "react";

export type Theme = "light" | "dark";

// Also read by the inline script in app/layout.tsx, which applies the theme
// before the first paint.
export const THEME_STORAGE_KEY = "spool-theme";
const DARK_QUERY = "(prefers-color-scheme: dark)";

function storedTheme(): Theme | null {
  try {
    const value = window.localStorage.getItem(THEME_STORAGE_KEY);
    return value === "dark" || value === "light" ? value : null;
  } catch {
    return null;
  }
}

function applyTheme(theme: Theme) {
  document.documentElement.setAttribute("data-theme", theme);
}

// The attribute on <html> is the one source of truth: the inline script sets
// it, the toggle changes it, and this hook only reads it.
function subscribe(onStoreChange: () => void): () => void {
  const observer = new MutationObserver(onStoreChange);
  observer.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });

  // Until the visitor chooses, the page follows the system as it changes.
  const query = window.matchMedia(DARK_QUERY);
  const followSystem = () => {
    if (!storedTheme()) applyTheme(query.matches ? "dark" : "light");
  };
  query.addEventListener("change", followSystem);

  return () => {
    observer.disconnect();
    query.removeEventListener("change", followSystem);
  };
}

function getSnapshot(): Theme {
  return document.documentElement.getAttribute("data-theme") === "dark" ? "dark" : "light";
}

function getServerSnapshot(): Theme {
  return "light";
}

export function setTheme(theme: Theme) {
  try {
    window.localStorage.setItem(THEME_STORAGE_KEY, theme);
  } catch {
    // Storage can be unavailable (private windows); the choice still applies
    // for this visit.
  }

  const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  if (!reducedMotion && "startViewTransition" in document) {
    // Which way the lamp is turning; the transition's CSS reads it.
    const root = document.documentElement;
    root.setAttribute("data-theme-change", theme);
    const transition = document.startViewTransition(() => applyTheme(theme));
    void transition.finished.finally(() => root.removeAttribute("data-theme-change"));
  } else {
    applyTheme(theme);
  }
}

export function useTheme(): Theme {
  return useSyncExternalStore(subscribe, getSnapshot, getServerSnapshot);
}
