/**
 * Platform detection for conditional UI rendering.
 *
 * On mobile (Android/iOS), sidecar options (Ollama, llama.cpp, MLX)
 * are hidden since subprocess spawning isn't available. The backend
 * commands still exist and compile — the UI simply doesn't expose them.
 */

/** True when running on Android or iOS (Tauri mobile). */
export function isMobile(): boolean {
    // Tauri injects `__TAURI_INTERNALS__` on all platforms.
    // On mobile, the user-agent or navigator.platform hints at Android/iOS.
    const ua = navigator.userAgent.toLowerCase();
    if (ua.includes('android') || ua.includes('iphone') || ua.includes('ipad')) return true;
    // iPadOS' WKWebView reports a *Macintosh* user-agent, so the check above
    // misses iPads and mobile-only UI leaks through. Touch points disambiguate:
    // macOS reports 0 (a trackpad is not a touchscreen), iPadOS reports 5.
    return ua.includes('mac') && (navigator.maxTouchPoints ?? 0) > 1;
}

/** True when running on a desktop OS. */
export function isDesktop(): boolean {
    return !isMobile();
}

/** Returns the platform name: 'android', 'ios', 'macos', 'windows', 'linux'. */
export function platformName(): string {
    const ua = navigator.userAgent.toLowerCase();
    if (ua.includes('android')) return 'android';
    if (ua.includes('iphone') || ua.includes('ipad')) return 'ios';
    if (ua.includes('mac')) return 'macos';
    if (ua.includes('win')) return 'windows';
    return 'linux';
}
