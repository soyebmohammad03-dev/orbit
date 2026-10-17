/** Base URL of the ORBIT API, e.g. `http://127.0.0.1:7878/api/v1`. */
export const API_BASE_URL: string = (process.env.NEXT_PUBLIC_ORBIT_API_URL ?? "http://127.0.0.1:7878/api/v1").replace(
    /\/+$/,
    "",
);
