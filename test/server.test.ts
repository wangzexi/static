import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import {
  createStaticServer,
  normalizeHost,
} from "../src/server.ts";

interface StoredObject {
  body: string;
  headers: Record<string, string>;
}

const objects = new Map<string, StoredObject>([
  [
    "/sites/zexi.me/index.html",
    {
      body: "<h1>home</h1>",
      headers: {
        "content-type": "text/html",
        etag: '"home-v1"',
      },
    },
  ],
  [
    "/sites/zexi.me/articles/hello/index.html",
    {
      body: "<h1>hello</h1>",
      headers: {
        "content-type": "text/html",
        etag: '"hello-v1"',
      },
    },
  ],
  [
    "/sites/zexi.me/assets/app.Dewnqifn.js",
    {
      body: "console.log('ok')",
      headers: {
        "content-type": "text/javascript",
        etag: '"asset-v1"',
      },
    },
  ],
  [
    "/sites/zexi.me/404.html",
    {
      body: "<h1>missing</h1>",
      headers: { "content-type": "text/html" },
    },
  ],
]);

let storage: Bun.Server<undefined>;
let gateway: Bun.Server<undefined>;
let gatewayUrl: string;

before(() => {
  storage = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    fetch(request) {
      const object = objects.get(new URL(request.url).pathname);
      if (!object) return new Response(null, { status: 404 });
      if (
        object.headers.etag &&
        request.headers.get("if-none-match") === object.headers.etag
      ) {
        return new Response(null, {
          status: 304,
          headers: object.headers,
        });
      }
      return new Response(request.method === "HEAD" ? null : object.body, {
        headers: object.headers,
      });
    },
  });

  gateway = createStaticServer({
    endpoint: `http://127.0.0.1:${storage.port}`,
    bucket: "sites",
    hostname: "127.0.0.1",
    port: 0,
  });
  gatewayUrl = `http://127.0.0.1:${gateway.port}`;
});

after(async () => {
  await Promise.all([gateway.stop(true), storage.stop(true)]);
});

interface RequestOptions {
  method?: string;
  headers?: Record<string, string>;
  body?: string;
}

function request(
  path: string,
  options: RequestOptions = {},
): Promise<Response> {
  return fetch(new URL(path, gatewayUrl), {
    method: options.method || "GET",
    headers: {
      host: "zexi.me",
      ...options.headers,
    },
    body: options.body,
    redirect: "manual",
  });
}

test("normalizes valid host names and rejects unsafe ones", () => {
  assert.equal(normalizeHost("ZEXI.ME:443"), "zexi.me");
  assert.equal(normalizeHost("zexi.me."), "zexi.me");
  assert.equal(normalizeHost("localhost"), null);
  assert.equal(normalizeHost("../zexi.me"), null);
});

test("serves the site index by Host", async () => {
  const response = await request("/");
  assert.equal(response.status, 200);
  assert.equal(response.headers.get("cache-control"), "no-cache");
  assert.equal(response.headers.get("etag"), '"home-v1"');
  assert.equal(await response.text(), "<h1>home</h1>");
});

test("supports HEAD without returning a body", async () => {
  const response = await request("/", { method: "HEAD" });
  assert.equal(response.status, 200);
  assert.equal(response.headers.get("etag"), '"home-v1"');
  assert.equal(await response.text(), "");
});

test("redirects extensionless directory paths to a trailing slash", async () => {
  const response = await request("/articles/hello?from=test");
  assert.equal(response.status, 308);
  assert.equal(response.headers.get("location"), "/articles/hello/?from=test");
});

test("serves directory index files", async () => {
  const response = await request("/articles/hello/");
  assert.equal(response.status, 200);
  assert.equal(await response.text(), "<h1>hello</h1>");
});

test("uses immutable caching for hashed assets", async () => {
  const response = await request("/assets/app.Dewnqifn.js");
  assert.equal(response.status, 200);
  assert.equal(
    response.headers.get("cache-control"),
    "public, max-age=31536000, immutable",
  );
});

test("forwards ETag validators", async () => {
  const response = await request("/", {
    headers: { "if-none-match": '"home-v1"' },
  });
  assert.equal(response.status, 304);
});

test("returns a custom 404 page without SPA fallback", async () => {
  const response = await request("/missing");
  assert.equal(response.status, 404);
  assert.equal(await response.text(), "<h1>missing</h1>");
});

test("rejects writes", async () => {
  const response = await request("/", { method: "PUT", body: "no" });
  assert.equal(response.status, 405);
});
