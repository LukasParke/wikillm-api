FROM oven/bun:1

WORKDIR /app

COPY package.json bun.lockb tsconfig.json ./
RUN bun install --frozen-lockfile

COPY . .

ENV NODE_ENV=production
ENV PORT=3000
ENV HOST=0.0.0.0

EXPOSE 3000

VOLUME ["/wiki"]

CMD ["bun", "run", "src/index.ts"]
