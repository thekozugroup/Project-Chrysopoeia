import type { NextConfig } from "next";

/**
 * The UI is exported as static files (web/out) and served by the Rust
 * binary. Everything is one page with hash routes, so the server only ever
 * needs to hand out index.html plus the /_next assets.
 */
const nextConfig: NextConfig = {
  output: "export",
  images: { unoptimized: true },
  poweredByHeader: false,
  reactStrictMode: true,
};

export default nextConfig;
