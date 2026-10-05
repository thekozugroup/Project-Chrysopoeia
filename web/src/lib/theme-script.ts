/**
 * Theme constants shared by the server-rendered layout and the client.
 * Kept free of "use client" so the layout can inline the boot script.
 */

/** localStorage key holding "light" or "dark" (absent = follow the OS). */
export const THEME_STORAGE_KEY = "szalinski-theme";

/** The key before the rename from Chrysopoeia, read when the current one is unset. */
export const LEGACY_THEME_STORAGE_KEY = "chrysopoeia-theme";

/** Script run in <head> before the app loads. Keep in sync with applyTheme. */
export const THEME_BOOT_SCRIPT = `(function(){try{var c=localStorage.getItem("${THEME_STORAGE_KEY}")||localStorage.getItem("${LEGACY_THEME_STORAGE_KEY}");var d=c==="dark"||(c!=="light"&&window.matchMedia("(prefers-color-scheme: dark)").matches);var r=document.documentElement;r.classList.toggle("dark",d);r.style.colorScheme=d?"dark":"light";}catch(e){document.documentElement.classList.add("dark");}})();`;
