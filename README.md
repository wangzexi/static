# static

> 退役说明：zexi.me 首页服务已经迁移到 [`wangzexi/zexi-me`](https://github.com/wangzexi/zexi-me)。本仓库只保留历史版本和回滚参考，不再作为集群首页镜像的源码入口。

面向个人 K3s 集群的极小静态网站网关，使用 Rust、Axum 和 reqwest
实现。它把请求的 Host 直接映射到 S3 bucket 中的同名目录，不维护站点
配置文件，也不提供 SPA fallback。

~~~text
GET https://zexi.me/articles/hello/
  -> s3://sites/zexi.me/articles/hello/index.html
~~~

## 约定

- bucket 默认叫 sites。
- 每个域名对应一个同名目录。
- 只支持 GET 和 HEAD。
- / 与以 / 结尾的路径读取目录下的 index.html。
- 无扩展名路径若存在目录首页，会重定向到带 / 的规范地址。
- 找不到对象时尝试站点根目录的 404.html，仍返回 HTTP 404。
- HTML 使用 Cache-Control: no-cache。
- 文件名包含至少 8 位构建哈希的资源使用长期 immutable 缓存。
- MinIO bucket 需允许匿名 GetObject，但不应允许匿名列目录或写入。

## 环境变量

zexi.me 的 `/llms.txt` 动态输出 Markdown 内容流，支持 `offset`（默认 0）、
`limit`（默认 20，范围 1–100）。请求从内存快照读取内容，普通分页链接携带这两个参数。
启动时和每日北京时间 06:00 从 MinIO 刷新完整快照，不依赖数据库，也不在镜像中打包内容。
更新内容只需发布静态文件；修改网关逻辑才需要重新构建镜像。
旧 `/llm`、`/llm.md`、`/index.md`、`/rss.xml`、`/articles.json` 返回 410。

~~~text
PORT=8080
S3_ENDPOINT=http://minio.minio.svc.cluster.local:9000
S3_BUCKET=sites
~~~

## 本地运行

~~~bash
cargo test
cargo run
~~~

HTTP 层使用 Axum，MinIO 请求使用 reqwest。生产镜像使用 musl 静态链接
二进制和 scratch，运行时不包含 shell、包管理器或语言运行时。
# Notes cache

`zexi.me` homepage, `/llms.txt` and `/feed/runtime/{version}/page-N.json` are rendered by this same Rust process from `sites/zexi.me/feed/all.json`. Startup and 06:00 Asia/Shanghai refresh the complete in-memory cache. Requests never fetch feed data from S3. Refresh failure retains the previous valid cache; one older version is retained for active scrolling sessions. No database or extra runtime is required.

Presentation assets are `notes-template.html` and `notes-emojis.json`, built by the blog frontend's `npm run build:runtime`. Daily content publishing changes only JSON/media, not the image or HTML shell. Resume and other hosts keep their static serving behavior.

## Historical note search

The homepage searches all cached note bodies, titles, and quoted text; the resume is excluded. `/feed/search.json?q=AI` returns rendered results with `total`, `offset`, `nextOffset`, and `snapshotId`. `/llms.txt?q=AI` returns the same matches as Markdown. Both accept `regex=1` for a Rust regular expression, `offset` (default 0), `limit` (default 20, range 1–100), and optional `snapshot` for consistent pagination across a refresh. Markdown next-page links preserve the query and snapshot. With no `q`, `/llms.txt` returns the first 20 notes, matching the homepage; its introduction shows pagination/search URLs and its footer links to the next page.

Keyword search is a literal, case-insensitive substring match. Regular expressions are also case-insensitive by default, support alternation such as `贝叶斯|概率`, and do not support look-around or backreferences. Queries are limited to 2048 UTF-8 bytes and compiled expressions to 1 MB. Invalid queries return 400; expired snapshots return 409. Search uses only the existing in-memory cache and never invokes a model or reads S3 at request time.
