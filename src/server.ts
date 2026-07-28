import http from "node:http";
import { Readable } from "node:stream";
import type { IncomingMessage, ServerResponse } from "node:http";

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

export function normalizeHost(value: string | undefined): string | null {
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

export function storageHostFor(host: string): string {
  return host.startsWith("www.") ? host.slice(4) : host;
}

function decodePathname(value: string): string | null {
  let pathname;
  try {
    pathname = decodeURIComponent(new URL(value, "http://static.invalid").pathname);
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

function requestHeaders(req: IncomingMessage): Headers {
  const headers = new Headers();
  for (const name of FORWARDED_REQUEST_HEADERS) {
    const value = req.headers[name];
    if (typeof value === "string") headers.set(name, value);
  }
  return headers;
}

async function fetchObject(
  context: StaticContext,
  req: IncomingMessage,
  host: string,
  pathname: string,
): Promise<Response> {
  const url = new URL(context.endpoint);
  const basePath = context.endpoint.pathname.replace(/\/+$/, "");
  url.pathname = `${basePath}/${encodeURIComponent(context.bucket)}/${encodeObjectKey(host, pathname)}`;
  url.search = "";

  return context.fetch(url, {
    method: req.method,
    headers: requestHeaders(req),
    redirect: "manual",
  });
}

function copyHeaders(
  upstream: Response,
  res: ServerResponse,
  pathname: string,
): void {
  for (const name of FORWARDED_RESPONSE_HEADERS) {
    const value = upstream.headers.get(name);
    if (value) res.setHeader(name, value);
  }
  res.setHeader(
    "cache-control",
    cacheControlFor(pathname, upstream.headers.get("cache-control")),
  );
  res.setHeader("x-content-type-options", "nosniff");
}

async function sendUpstream(
  upstream: Response,
  req: IncomingMessage,
  res: ServerResponse,
  pathname: string,
  status = upstream.status,
): Promise<void> {
  res.statusCode = status;
  copyHeaders(upstream, res, pathname);

  const body = upstream.body;
  if (req.method === "HEAD" || status === 304 || !body) {
    res.end();
    return;
  }

  await new Promise((resolve, reject) => {
    Readable.fromWeb(
      body as unknown as import("node:stream/web").ReadableStream,
    )
      .on("error", reject)
      .pipe(res)
      .on("finish", resolve)
      .on("error", reject);
  });
}

function plain(res: ServerResponse, status: number, message: string): void {
  res.writeHead(status, {
    "cache-control": "no-store",
    "content-type": "text/plain; charset=utf-8",
    "x-content-type-options": "nosniff",
  });
  res.end(`${message}\n`);
}

export function createHandler(options: StaticOptions = {}) {
  const context: StaticContext = {
    endpoint: normalizeEndpoint(
      options.endpoint || process.env.S3_ENDPOINT || "http://minio.minio.svc.cluster.local:9000",
    ),
    bucket: normalizeBucket(options.bucket || process.env.S3_BUCKET || "sites"),
    fetch: options.fetch || globalThis.fetch,
  };

  return async function handler(
    req: IncomingMessage,
    res: ServerResponse,
  ): Promise<void> {
    try {
      if (req.url === "/healthz" || req.url === "/readyz") {
        plain(res, 200, "ok");
        return;
      }
      if (req.method !== "GET" && req.method !== "HEAD") {
        res.setHeader("allow", "GET, HEAD");
        plain(res, 405, "method not allowed");
        return;
      }

      const host = normalizeHost(req.headers.host);
      const pathname = decodePathname(req.url ?? "/");
      if (!host || pathname === null) {
        plain(res, 400, "bad request");
        return;
      }
      const storageHost = storageHostFor(host);

      const requestedPath = pathname.endsWith("/")
        ? `${pathname}index.html`
        : pathname;
      let upstream = await fetchObject(context, req, storageHost, requestedPath);

      if (
        upstream.status === 404 &&
        !pathname.endsWith("/") &&
        !(pathname.split("/").at(-1)?.includes(".") ?? false)
      ) {
        upstream.body?.cancel();
        const indexPath = `${pathname}/index.html`;
        upstream = await fetchObject(context, req, storageHost, indexPath);
        if (upstream.ok) {
          upstream.body?.cancel();
          res.writeHead(308, {
            "cache-control": "no-cache",
            location: `${pathname}/${new URL(req.url ?? "/", "http://static.invalid").search}`,
          });
          res.end();
          return;
        }
      }

      if (upstream.status === 404) {
        upstream.body?.cancel();
        const notFound = await fetchObject(
          context,
          req,
          storageHost,
          "/404.html",
        );
        if (notFound.ok) {
          await sendUpstream(notFound, req, res, "/404.html", 404);
          return;
        }
        notFound.body?.cancel();
        plain(res, 404, "not found");
        return;
      }

      if (!upstream.ok && upstream.status !== 304 && upstream.status !== 206) {
        upstream.body?.cancel();
        plain(res, 502, "storage unavailable");
        return;
      }

      await sendUpstream(upstream, req, res, requestedPath);
    } catch (error: unknown) {
      console.error(error);
      if (!res.headersSent) plain(res, 502, "storage unavailable");
      else res.destroy();
    }
  };
}

export function createStaticServer(options: StaticOptions = {}): http.Server {
  return http.createServer(createHandler(options));
}

if (process.argv[1] && import.meta.url === `file://${process.argv[1]}`) {
  const port = Number.parseInt(process.env.PORT || `${DEFAULT_PORT}`, 10);
  const server = createStaticServer();
  server.listen(port, "0.0.0.0", () => {
    console.log(`static listening on :${port}`);
  });
}
