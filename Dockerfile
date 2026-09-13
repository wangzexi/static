FROM rust:1.85-alpine AS build

WORKDIR /app
RUN apk add --no-cache build-base musl-dev

COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN rustup target add x86_64-unknown-linux-musl
RUN cargo build --release --target x86_64-unknown-linux-musl

FROM scratch

COPY --from=build /app/target/x86_64-unknown-linux-musl/release/static-gateway /static-gateway

USER 1000:1000
EXPOSE 8080
ENTRYPOINT ["/static-gateway"]
