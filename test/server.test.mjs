import assert from "node:assert/strict";
import http from "node:http";
import { after, before, test } from "node:test";
import { createStaticServer, normalizeHost } from "../src/server.mjs";

const objects = new Map([
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
    "/sites/zexi.me/assets/app.12345678.js",
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
let storage;
let gateway;
let storageUrl;
let gatewayUrl;

before(async () => {
  storage = http.createServer((req, res) => {
    const object = objects.get(req.url);
    if (!object) {
      res.writeHead(404);
      res.end();
      return;
    }
    if (
      object.headers.etag &&
      req.headers["if-none-match"] === object.headers.etag
    ) {
      res.writeHead(304, object.headers);
      res.end();
      return;
    }
    res.writeHead(200, object.headers);
    res.end(req.method === "HEAD" ? undefined : object.body);
  });
  await new Promise((resolve) => storage.listen(0, "127.0.0.1", resolve));
  storageUrl = `http://127.0.0.1:${storage.address().port}`;

  gateway = createStaticServer({ endpoint: storageUrl, bucket: "sites" });
  await new Promise((resolve) => gateway.listen(0, "127.0.0.1", resolve));
  gatewayUrl = `http://127.0.0.1:${gateway.address().port}`;
});

after(async () => {
  await Promise.all([
    new Promise((resolve) => gateway.close(resolve)),
    new Promise((resolve) => storage.close(resolve)),
  ]);
});

async function request(path, options = {}) {
  const url = new URL(path, gatewayUrl);
  return new Promise((resolve, reject) => {
    const req = http.request({
      hostname: url.hostname,
      port: url.port,
      path: `${url.pathname}${url.search}`,
      method: options.method || "GET",
      headers: {
        host: "zexi.me",
        ...options.headers,
      },
    });
    req.on("error", reject);
    req.on("response", (res) => {
      const chunks = [];
      res.on("data", (chunk) => chunks.push(chunk));
      res.on("end", () => {
        const body = Buffer.concat(chunks);
        resolve({
          status: res.statusCode,
          headers: {
            get(name) {
              const value = res.headers[name.toLowerCase()];
              return Array.isArray(value) ? value.join(", ") : value ?? null;
            },
          },
          async text() {
            return body.toString();
          },
        });
      });
    });
    if (options.body) req.write(options.body);
    req.end();
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
  const response = await request("/assets/app.12345678.js");
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
