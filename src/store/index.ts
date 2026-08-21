import type { Config } from "../config.js";
import { createSqliteDatabase, SqliteStore } from "./sqlite.js";
import { PostgresStore } from "./pg.js";
import type { Store } from "./types.js";

export async function createStore(config: Config): Promise<Store> {
  const backend =
    config.DB_BACKEND === "auto"
      ? config.DATABASE_URL
        ? "postgres"
        : "sqlite"
      : config.DB_BACKEND;

  if (backend === "postgres") {
    if (!config.DATABASE_URL) {
      throw new Error("DATABASE_URL is required when DB_BACKEND=postgres");
    }
    const store = await PostgresStore.connect(config.DATABASE_URL);
    await store.migrate();
    return store;
  }

  const db = await createSqliteDatabase(config.DB_PATH ?? "wikillm-api.db");
  const store = new SqliteStore(db);
  await store.migrate();
  return store;
}

export type { Store } from "./types.js";
