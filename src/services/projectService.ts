import type { Store } from "../store/types.js";
import type { ProjectInput } from "../store/types.js";

export interface AuthInfo {
  name: string;
  role: "admin" | "write" | "read";
  /** project names this identity may access; ["*"] = all */
  projects: string[];
}

export class ForbiddenError extends Error {
  constructor(message = "Forbidden") {
    super(message);
    this.name = "ForbiddenError";
  }
}

const ROLE_RANK: Record<AuthInfo["role"], number> = {
  read: 0,
  write: 1,
  admin: 2,
};

export function roleAtLeast(
  role: AuthInfo["role"],
  min: AuthInfo["role"],
): boolean {
  return ROLE_RANK[role] >= ROLE_RANK[min];
}

/**
 * Resolve the path-prefix filter for a request. Anonymous/all-access returns
 * ["*"]; a named project intersects the caller's allowed projects.
 */
export async function resolveScopePrefixes(
  store: Store,
  auth: AuthInfo,
  requestedProject?: string,
): Promise<string[]> {
  if (requestedProject && requestedProject !== "*") {
    if (
      !auth.projects.includes("*") &&
      !auth.projects.includes(requestedProject)
    ) {
      throw new ForbiddenError(`Project not allowed: ${requestedProject}`);
    }
    const project = await store.getProject(requestedProject);
    if (!project) return ["__none__"];
    return projectPrefixes(project);
  }
  if (auth.projects.includes("*")) return ["*"];
  if (auth.projects.length === 0) return ["__none__"];
  const prefixes: string[] = [];
  for (const name of auth.projects) {
    const project = await store.getProject(name);
    if (project) prefixes.push(...projectPrefixes(project));
  }
  return prefixes.length > 0 ? prefixes : ["__none__"];
}

function projectPrefixes(project: {
  prefixes: string[];
  connectors: string[];
}): string[] {
  return [...project.prefixes, ...project.connectors.map((c) => `${c}/`)];
}

export function createProjectService(store: Store) {
  return {
    list: () => store.listProjects(),
    get: (name: string) => store.getProject(name),
    put: (input: ProjectInput) => store.putProject(input),
    delete: (name: string) => store.deleteProject(name),
  };
}
