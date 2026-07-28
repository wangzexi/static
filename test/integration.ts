import assert from "node:assert/strict";
import { createStaticServer } from "../src/server.ts";

const endpoint = process.env.S3_ENDPOINT;
if (!endpoint) {
  throw new Error("S3_ENDPOINT is required");
}

const gateway = createStaticServer({
  endpoint,
  bucket: process.env.S3_BUCKET || "sites",
  hostname: "127.0.0.1",
  port: 0,
});

interface RequestOptions {
  method?: string;
  headers?: Record<string, string>;
  body?: string;
}

function request(
  host: string,
  path: string,
  options: RequestOptions = {},
): Promise<Response> {
  return fetch(`http://127.0.0.1:${gateway.port}${path}`, {
    method: options.method || "GET",
    headers: {
      host,
      ...options.headers,
    },
    body: options.body,
    redirect: "manual",
  });
}

try {
  const roots = [
    ["zexi.me", "思考的扰动"],
    ["wzx6.cn", "站点建设中"],
    ["cheer.world", "Cheer 星球"],
    ["zexi.love", "站点建设中"],
  ];
  for (const [host, text] of roots) {
    const response = await request(host, "/");
    assert.equal(response.status, 200, `${host} root status`);
    assert.match(await response.text(), new RegExp(text), `${host} root body`);
  }

  const article = await request(
    "zexi.me",
    `/${encodeURIComponent("对大语言模型（LLM）应用的理解")}/`,
  );
  assert.equal(article.status, 200);
  assert.match(await article.text(), /大语言模型/);

  const asset = await request(
    "cheer.world",
    "/assets/index-Dewnqifn.js",
  );
  assert.equal(asset.status, 200);
  assert.match(asset.headers.get("content-type") ?? "", /javascript/);
  assert.equal(
    asset.headers.get("cache-control"),
    "public, max-age=31536000, immutable",
  );
  await asset.body?.cancel();

  const head = await request("zexi.me", "/", { method: "HEAD" });
  assert.equal(head.status, 200);
  assert.equal(await head.text(), "");

  const range = await request(
    "cheer.world",
    "/assets/index-Dewnqifn.js",
    { headers: { range: "bytes=0-9" } },
  );
  assert.equal(range.status, 206);
  assert.match(range.headers.get("content-range") ?? "", /^bytes 0-9\//);
  assert.equal((await range.bytes()).byteLength, 10);

  const root = await request("zexi.me", "/");
  assert.equal(root.headers.get("cache-control"), "no-cache");
  const etag = root.headers.get("etag");
  if (!etag) throw new Error("zexi.me ETag is missing");
  await root.body?.cancel();

  const conditional = await request("zexi.me", "/", {
    headers: { "if-none-match": etag },
  });
  assert.equal(conditional.status, 304);

  const missing = await request("zexi.me", "/definitely-missing");
  assert.equal(missing.status, 404);

  const write = await request("zexi.me", "/", {
    method: "PUT",
    body: "nope",
  });
  assert.equal(write.status, 405);

  console.log(
    "integration checks passed: roots=4 article=1 asset=1 head=1 range=1 etag=1 404=1 method=1",
  );
} finally {
  await gateway.stop(true);
}
