FROM oven/bun:1 AS base

WORKDIR /app

FROM base AS install
COPY package.json bun.lock tsconfig.json ./
RUN bun install --frozen-lockfile

FROM base AS release
COPY --from=install /app/node_modules ./node_modules
COPY package.json bun.lock tsconfig.json ./
COPY . .

ENV NODE_ENV=production
ENV PORT=3000
ENV HOST=0.0.0.0

EXPOSE 3000

VOLUME ["/wiki"]

CMD ["bun", "run", "src/index.ts"]
