# okbot in a container: one container per bot.
#
#   docker build -t okbot --build-arg AGENT_CLIS="@anthropic-ai/claude-code" .
#   docker run -it --rm -v okbot-home:/home/okbot okbot claude          # log in once
#   docker run -d --name shop -v okbot-home:/home/okbot -v $PWD/shop:/bot okbot
#
# /bot is the bot directory (okbot.toml, .env, kb/, state/), created with `okbot new` on the
# host or with `docker run -it -v $PWD/shop:/bot okbot okbot init`.
# /home/okbot keeps the agent CLIs' logins.

FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p okbot-cli

FROM debian:bookworm-slim
ARG AGENT_CLIS=""
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl nodejs npm \
 && if [ -n "$AGENT_CLIS" ]; then npm install -g $AGENT_CLIS; fi \
 && apt-get clean && rm -rf /var/lib/apt/lists/* \
 && useradd --create-home --uid 10001 okbot
COPY --from=build /src/target/release/okbot /usr/local/bin/okbot
USER okbot
WORKDIR /bot
VOLUME ["/bot", "/home/okbot"]
ENV OKBOT_HOME=/home/okbot/.config/okbot
CMD ["okbot", "run"]
