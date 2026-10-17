import type { NextConfig } from "next";

const nextConfig: NextConfig = {
    // The dev-mode indicator overlaps the sidebar's API health status.
    devIndicators: false,
    cacheComponents: true,
    partialPrefetching: true,
    turbopack: {
        rules: {
            "*.css": {
                loaders: ["@tailwindcss/turbopack"],
                as: "*.css",
            },
        },
    },
};

export default nextConfig;
