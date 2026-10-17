import type { NextConfig } from "next";

// A static site: `next build` writes plain HTML, CSS and JS to `out/`, which
// Vercel (or any static host) serves as is.
const nextConfig: NextConfig = {
  output: "export",
  images: { unoptimized: true },
};

export default nextConfig;
