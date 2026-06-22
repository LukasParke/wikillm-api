export type Source = string;

export interface Page {
  rel_path: string;
  abs_path: string;
  title: string | null;
  summary: string | null;
  frontmatter: Record<string, unknown>;
  body: string;
  word_count: number;
  outgoing_links: string[];
  hash: string;
  mtime: number;
  updated_at: string | null;
  updated_by: Source | null;
}

export interface SourceFile {
  rel_path: string;
  abs_path: string;
  content_type: string | null;
  size: number;
  hash: string;
  mtime: number;
}

export type ChangeType = "created" | "modified" | "deleted" | "renamed";

export interface ChangeEvent {
  type: "change";
  data: {
    id: string;
    rel_path: string;
    change_type: ChangeType;
    old_hash: string | null;
    new_hash: string | null;
    source: "api" | "external" | null;
    operation_id: string | null;
    detected_at: string;
  };
}

export interface Operation {
  id: string;
  created_at: string;
  source: Source;
  action: string;
  paths: string[];
  metadata: Record<string, unknown> | null;
  parent_id: string | null;
}

export interface PageWriteInput {
  rel_path: string;
  content: string;
  frontmatter?: Record<string, unknown>;
  ifMatch?: string | null;
}

export interface AppContext {
  wikiRoot: string;
  dbPath: string;
  apiKeys: Map<string, Source>;
  publicRead: boolean;
  logLevel: string;
}
