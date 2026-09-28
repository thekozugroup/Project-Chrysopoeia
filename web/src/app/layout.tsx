import type { Metadata, Viewport } from "next";
import "@fontsource/instrument-serif/400.css";
import "@fontsource-variable/dm-sans/wght.css";
import "@fontsource-variable/jetbrains-mono/wght.css";
import "./globals.css";
import { Providers } from "@/components/providers";
import { THEME_BOOT_SCRIPT } from "@/lib/theme-script";

export const metadata: Metadata = {
  title: "Chrysopoeia",
  description:
    "Chrysopoeia converts your media library to efficient formats in the background, verifies every file, and only then replaces the original.",
  applicationName: "Chrysopoeia",
  robots: { index: false, follow: false },
};

export const viewport: Viewport = {
  width: "device-width",
  initialScale: 1,
  viewportFit: "cover",
  themeColor: [
    { media: "(prefers-color-scheme: dark)", color: "#1b1814" },
    { media: "(prefers-color-scheme: light)", color: "#f7f4ee" },
  ],
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en" suppressHydrationWarning>
      <head>
        {/* Apply the saved theme before first paint. */}
        <script dangerouslySetInnerHTML={{ __html: THEME_BOOT_SCRIPT }} />
      </head>
      <body>
        <Providers>{children}</Providers>
      </body>
    </html>
  );
}
