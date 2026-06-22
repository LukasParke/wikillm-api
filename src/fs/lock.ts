export class PathLock {
  private queues = new Map<
    string,
    Array<{ resolve: (release: () => void) => void }>
  >();
  private held = new Set<string>();

  async acquire(relPath: string): Promise<() => void> {
    if (!this.held.has(relPath)) {
      this.held.add(relPath);
      return () => this.release(relPath);
    }

    return new Promise<() => void>((resolve) => {
      const queue = this.queues.get(relPath) ?? [];
      queue.push({ resolve });
      this.queues.set(relPath, queue);
    });
  }

  private release(relPath: string): void {
    const queue = this.queues.get(relPath);
    if (queue && queue.length > 0) {
      const next = queue.shift()!;
      next.resolve(() => this.release(relPath));
    } else {
      this.held.delete(relPath);
      this.queues.delete(relPath);
    }
  }

  async runExclusive<T>(relPath: string, fn: () => Promise<T>): Promise<T> {
    const release = await this.acquire(relPath);
    try {
      return await fn();
    } finally {
      release();
    }
  }

  async acquireMany(relPaths: string[]): Promise<() => void> {
    const sorted = Array.from(new Set(relPaths)).slice().sort();
    const releases: (() => void)[] = [];
    try {
      for (const p of sorted) {
        releases.push(await this.acquire(p));
      }
    } catch (err) {
      releases.forEach((r) => r());
      throw err;
    }
    return () => {
      for (let i = releases.length - 1; i >= 0; i--) {
        releases[i]();
      }
    };
  }

  async runManyExclusive<T>(
    relPaths: string[],
    fn: () => Promise<T>,
  ): Promise<T> {
    const release = await this.acquireMany(relPaths);
    try {
      return await fn();
    } finally {
      release();
    }
  }
}

export const pathLock = new PathLock();
