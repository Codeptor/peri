import type { NextConfig } from "next"

const nextConfig: NextConfig = {
  transpilePackages: ["@workspace/ui"],
  async rewrites() {
    // Browser never crosses origins: /peri/* proxies to the peri daemon's
    // read-only API (127.0.0.1:7411). The daemon sends no CORS headers by design.
    return [
      { source: "/peri/:path*", destination: "http://127.0.0.1:7411/:path*" },
    ]
  },
}

export default nextConfig
