import type { Metadata } from "next";
import { Instrument_Serif, DM_Sans, JetBrains_Mono } from "next/font/google";
import "./globals.css";
import { Providers } from "@/components/providers";

const instrumentSerif = Instrument_Serif({
  variable: "--font-display",
  subsets: ["latin"],
  weight: "400",
});

const dmSans = DM_Sans({
  variable: "--font-sans",
  subsets: ["latin"],
});

const jetbrainsMono = JetBrains_Mono({
  variable: "--font-mono",
  subsets: ["latin"],
});

export const metadata: Metadata = {
  title: {
    default: "Chrysopeia",
    template: "%s | Chrysopeia",
  },
  description:
    "Transmute your media library. Chrysopeia is a conversational interface for intelligent media transcoding with hardware-accelerated encoding.",
  openGraph: {
    title: "Chrysopeia",
    description:
      "Transmute your media library with intelligent, conversational transcoding.",
    siteName: "Chrysopeia",
    type: "website",
  },
  other: {
    "theme-color": "#0d0d0d",
  },
};

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  return (
    <html
      lang="en"
      className={`${instrumentSerif.variable} ${dmSans.variable} ${jetbrainsMono.variable} dark h-full antialiased`}
    >
      <body className="h-full overflow-hidden bg-background text-foreground">
        <Providers>{children}</Providers>
      </body>
    </html>
  );
}
