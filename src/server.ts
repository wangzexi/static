const DEFAULT_PORT = 8080;
const FORWARDED_REQUEST_HEADERS = [
  "if-match",
  "if-modified-since",
  "if-none-match",
  "if-range",
  "range",
];
const FORWARDED_RESPONSE_HEADERS = [
  "accept-ranges",
  "content-encoding",
  "content-length",
  "content-range",
  "content-type",
  "etag",
  "last-modified",
];

type FetchLike = typeof globalThis.fetch;

export interface StaticOptions {
  endpoint?: string;
  bucket?: string;
  fetch?: FetchLike;
}

export interface StaticServerOptions extends StaticOptions {
  hostname?: string;
  port?: number;
}

interface StaticContext {
  endpoint: URL;
  bucket: string;
  fetch: FetchLike;
}

function normalizeEndpoint(value: string): URL {
  const endpoint = new URL(value);
  endpoint.pathname = endpoint.pathname.replace(/\/+$/, "");
  return endpoint;
}

function normalizeBucket(value: string): string {
  if (!/^[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]$/.test(value)) {
    throw new Error("S3_BUCKET must be a valid S3 bucket name");
  }
  return value;
}

export function normalizeHost(
  value: string | null | undefined,
): string | null {
  if (!value) return null;

  let host = value.trim().toLowerCase();
  if (host.startsWith("[")) return null;
  host = host.split(":", 1)[0].replace(/\.$/, "");

  if (
    host.length === 0 ||
    host.length > 253 ||
    !host.includes(".") ||
    !/^[a-z0-9.-]+$/.test(host) ||
    host.includes("..") ||
    host.split(".").some((label) => {
      return (
        label.length === 0 ||
        label.length > 63 ||
        label.startsWith("-") ||
        label.endsWith("-")
      );
    })
  ) {
    return null;
  }

  return host;
}

function decodePathname(value: string): string | null {
  let pathname;
  try {
    pathname = decodeURIComponent(value);
  } catch {
    return null;
  }

  if (
    pathname.includes("\0") ||
    pathname.includes("\\") ||
    pathname.split("/").includes("..")
  ) {
    return null;
  }
  return pathname;
}

function encodeObjectKey(host: string, pathname: string): string {
  const parts = [host, ...pathname.split("/").filter(Boolean)];
  return parts.map(encodeURIComponent).join("/");
}

function cacheControlFor(
  pathname: string,
  upstreamValue: string | null,
): string {
  if (pathname.endsWith(".html")) return "no-cache";
  if (
    pathname.startsWith("/assets/") ||
    pathname.startsWith("/_astro/") ||
    /(?:^|\/)[^/]*[.-][a-z0-9_-]{8,}[.-][^/]+$/i.test(pathname)
  ) {
    return "public, max-age=31536000, immutable";
  }
  return upstreamValue || "public, max-age=3600";
}

function requestHeaders(request: Request): Headers {
  const headers = new Headers();
  for (const name of FORWARDED_REQUEST_HEADERS) {
    const value = request.headers.get(name);
    if (value) headers.set(name, value);
  }
  return headers;
}

async function fetchObject(
  context: StaticContext,
  request: Request,
  host: string,
  pathname: string,
): Promise<Response> {
  const url = new URL(context.endpoint);
  const basePath = context.endpoint.pathname.replace(/\/+$/, "");
  url.pathname = `${basePath}/${encodeURIComponent(context.bucket)}/${encodeObjectKey(host, pathname)}`;
  url.search = "";

  return context.fetch(url, {
    method: request.method,
    headers: requestHeaders(request),
    redirect: "manual",
  });
}

function responseHeaders(upstream: Response, pathname: string): Headers {
  const headers = new Headers();
  for (const name of FORWARDED_RESPONSE_HEADERS) {
    const value = upstream.headers.get(name);
    if (value) headers.set(name, value);
  }
  headers.set(
    "cache-control",
    cacheControlFor(pathname, upstream.headers.get("cache-control")),
  );
  headers.set("x-content-type-options", "nosniff");
  return headers;
}

function sendUpstream(
  upstream: Response,
  request: Request,
  pathname: string,
  status = upstream.status,
): Response {
  const body =
    request.method === "HEAD" || status === 304 ? null : upstream.body;
  return new Response(body, {
    status,
    headers: responseHeaders(upstream, pathname),
  });
}

function plain(
  status: number,
  message: string,
  extraHeaders?: HeadersInit,
): Response {
  const headers = new Headers(extraHeaders);
  headers.set("cache-control", "no-store");
  headers.set("content-type", "text/plain; charset=utf-8");
  headers.set("x-content-type-options", "nosniff");
  return new Response(`${message}\n`, { status, headers });
}

export function createHandler(options: StaticOptions = {}) {
  const context: StaticContext = {
    endpoint: normalizeEndpoint(
      options.endpoint ||
        process.env.S3_ENDPOINT ||
        "http://minio.minio.svc.cluster.local:9000",
    ),
    bucket: normalizeBucket(options.bucket || process.env.S3_BUCKET || "sites"),
    fetch: options.fetch || globalThis.fetch,
  };

  return async function handler(request: Request): Promise<Response> {
    try {
      const requestUrl = new URL(request.url);
      if (
        requestUrl.pathname === "/healthz" ||
        requestUrl.pathname === "/readyz"
      ) {
        return plain(200, "ok");
      }
      if (request.method !== "GET" && request.method !== "HEAD") {
        return plain(405, "method not allowed", { allow: "GET, HEAD" });
      }

      const host = normalizeHost(request.headers.get("host"));
      const pathname = decodePathname(requestUrl.pathname);
      if (!host || pathname === null) {
        return plain(400, "bad request");
      }

      const requestedPath = pathname.endsWith("/")
        ? `${pathname}index.html`
        : pathname;
      let upstream = await fetchObject(context, request, host, requestedPath);

      if (
        upstream.status === 404 &&
        !pathname.endsWith("/") &&
        !(pathname.split("/").at(-1)?.includes(".") ?? false)
      ) {
        await upstream.body?.cancel();
        const indexPath = `${pathname}/index.html`;
        upstream = await fetchObject(context, request, host, indexPath);
        if (upstream.ok) {
          await upstream.body?.cancel();
          return new Response(null, {
            status: 308,
            headers: {
              "cache-control": "no-cache",
              location: `${requestUrl.pathname}/${requestUrl.search}`,
            },
          });
        }
      }

      if (upstream.status === 404) {
        await upstream.body?.cancel();
        const notFound = await fetchObject(
          context,
          request,
          host,
          "/404.html",
        );
        if (notFound.ok) {
          return sendUpstream(notFound, request, "/404.html", 404);
        }
        await notFound.body?.cancel();
        return plain(404, "not found");
      }

      if (!upstream.ok && upstream.status !== 304 && upstream.status !== 206) {
        await upstream.body?.cancel();
        return plain(502, "storage unavailable");
      }

      return sendUpstream(upstream, request, requestedPath);
    } catch (error: unknown) {
      console.error(error);
      return plain(502, "storage unavailable");
    }
  };
}

export function createStaticServer(options: StaticServerOptions = {}) {
  return Bun.serve({
    hostname: options.hostname || "0.0.0.0",
    port: options.port ?? DEFAULT_PORT,
    fetch: createHandler(options),
  });
}

if (import.meta.main) {
  const port = Number.parseInt(process.env.PORT || `${DEFAULT_PORT}`, 10);
  createStaticServer({ port });
  console.log(`static listening on :${port}`);
}
