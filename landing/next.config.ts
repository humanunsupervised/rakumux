import type { NextConfig } from "next";

// Public marketing is a static export. Hosted accounts and AI implementation
// belong to the separate private rakumux-cloud repository.
const nextConfig: NextConfig = {
  output: "export",
  images: { unoptimized: true },
};

export default nextConfig;
