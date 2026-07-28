# static

面向个人 K3s 集群的极小静态网站网关。它把请求的 `Host` 直接映射到
S3 bucket 中的同名目录，不维护站点配置文件，也不提供 SPA fallback。

```text
GET https://zexi.me/articles/hello/
  -> s3://sites/zexi.me/articles/hello/index.html
```

## 约定

- bucket 默认叫 `sites`。
- 每个域名对应一个同名目录。
- 只支持 `GET` 和 `HEAD`。
- `/` 与以 `/` 结尾的路径读取目录下的 `index.html`。
- 无扩展名路径若存在目录首页，会重定向到带 `/` 的规范地址。
- 找不到对象时尝试站点根目录的 `404.html`，仍返回 HTTP 404。
- HTML 使用 `Cache-Control: no-cache`。
- 文件名包含至少 8 位十六进制哈希的资源使用长期 immutable 缓存。
- MinIO bucket 需允许匿名 `GetObject`，但不应允许匿名列目录或写入。

## 环境变量

```text
PORT=8080
S3_ENDPOINT=http://minio.minio.svc.cluster.local:9000
S3_BUCKET=sites
```

## 本地运行

```bash
bun install
bun test
bun start
```

源码、单元测试和真实 MinIO 集成测试均使用 TypeScript，由 Bun 直接运行。
HTTP 层使用原生 `Bun.serve()` 和 Web 标准的 `Request`、`Response`、`ReadableStream`。
生产镜像使用 Bun distroless，运行时不安装 npm 依赖。
