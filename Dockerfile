FROM oven/bun:1.3.14-distroless

WORKDIR /app

COPY --chown=bun:bun package.json ./
COPY --chown=bun:bun src ./src

USER bun
EXPOSE 8080

ENTRYPOINT ["bun"]
CMD ["src/server.ts"]
