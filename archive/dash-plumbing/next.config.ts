import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  // Same-origin proxy so the browser never crosses origins (traderd stays bound to localhost).
  // lib/api.ts uses "./traderd" client-side and the absolute URL server-side.
  rewrites() {
    return [
      { source: "/traderd/:path*", destination: "http://127.0.0.1:7411/:path*" },
    ];
  },
};

export default nextConfig;
