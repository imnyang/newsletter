FROM rust:slim as builder

WORKDIR /usr/src/app
COPY . .

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
RUN --mount=type=cache,target=/usr/local/cargo/registry cargo build --release

FROM debian:13-slim

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

WORKDIR /usr/app
COPY --from=builder /usr/src/app/target/release/newsletter .

CMD ["./newsletter"]