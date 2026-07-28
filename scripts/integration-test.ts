import assert from "node:assert/strict";
import http from "node:http";
import type { AddressInfo } from "node:net";
import { createStaticServer } from "../src/server.ts";

const endpoint = process.env.S3_ENDPOINT;
if (!endpoint) {
  throw new Error("S3_ENDPOINT is required");
}

const gateway = createStaticServer({
  endpoint,
  bucket: process.env.S3_BUCKET || "sites",
});

await new Promise<void>((resolve, reject) => {
  gateway.once("error", reject);
  gateway.listen(0, "127.0.0.1", () => resolve());
});

interface RequestOptions {
  method?: string;
  headers?: Record<string, string>;
  body?: string;
}

interface IntegrationResponse {
  status: number | undefined;
  headers: http.IncomingHttpHeaders;
  body: string;
}

function request(
  host: string,
  path: string,
  options: RequestOptions = {},
): Promise<IntegrationResponse> {
  return new Promise((resolve, reject) => {
    const req = http.request({
      hostname: "127.0.0.1",
      port: (gateway.address() as AddressInfo).port,
      path,
      method: options.method || "GET",
      headers: {
        host,
        ...options.headers,
      },
    });
    req.on("error", reject);
    req.on("response", (res) => {
      const chunks: Buffer[] = [];
      res.on("data", (chunk: Buffer) => chunks.push(chunk));
      res.on("end", () => {
        resolve({
          status: res.statusCode,
          headers: res.headers,
          body: Buffer.concat(chunks).toString(),
        });
      });
    });
    if (options.body) req.write(options.body);
    req.end();
  });
}

try {
  const roots = [
    ["zexi.me", "思考的扰动"],
    ["wzx6.cn", "站点建设中"],
    ["cheer.world", "Cheer 星球"],
  ];
  for (const [host, text] of roots) {
    const response = await request(host, "/");
    assert.equal(response.status, 200, `${host} root status`);
    assert.match(response.body, new RegExp(text), `${host} root body`);
  }

  const article = await request(
    "zexi.me",
    `/${encodeURIComponent("对大语言模型（LLM）应用的理解")}/`,
  );
  assert.equal(article.status, 200);
  assert.match(article.body, /大语言模型/);

  const asset = await request(
    "cheer.world",
    "/assets/index-Dewnqifn.js",
  );
  assert.equal(asset.status, 200);
  assert.match(asset.headers["content-type"] ?? "", /javascript/);
  assert.equal(
    asset.headers["cache-control"],
    "public, max-age=31536000, immutable",
  );

  const root = await request("zexi.me", "/");
  assert.equal(root.headers["cache-control"], "no-cache");
  const etag = root.headers.etag;
  if (typeof etag !== "string") throw new Error("zexi.me ETag is missing");

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
    "integration checks passed: roots=3 article=1 asset=1 etag=1 404=1 method=1",
  );
} finally {
  gateway.closeAllConnections();
  await new Promise((resolve) => gateway.close(resolve));
}
