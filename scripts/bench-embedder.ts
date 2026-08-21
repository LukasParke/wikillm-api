import { createOnnxEmbedder } from "../src/llm/embedder.js";

const model = process.argv[2] ?? "Xenova/bge-small-en-v1.5";
const n = Number(process.argv[3] ?? 256);

console.log(`Loading ${model} (q8, cpu)...`);
const t0 = Date.now();
const embedder = createOnnxEmbedder({ model, dtype: "q8", device: "cpu" });

// warm-up (includes model download on first run)
await embedder.embed(["warm up"]);
console.log(`Model ready in ${Date.now() - t0}ms`);

const texts = Array.from(
  { length: n },
  (_, i) =>
    `Entity ${i} operates in the LLM ecosystem providing inference infrastructure and tooling for retrieval workloads, related to overview page ${i % 50}.`,
);

const started = Date.now();
const vectors = await embedder.embed(texts);
const elapsed = Date.now() - started;

console.log(`Embedded ${n} texts in ${elapsed}ms -> ${(n / (elapsed / 1000)).toFixed(0)} chunks/s`);
console.log(`dims: ${embedder.dims}, vector length: ${vectors[0].length}`);

// sanity: cosine similarity of related vs unrelated
function dot(a: number[], b: number[]): number {
  let s = 0;
  for (let i = 0; i < a.length; i++) s += a[i] * b[i];
  return s;
}
const [q] = await embedder.embed(["How fast is semantic retrieval for LLM knowledge bases?"]);
const near = await embedder.embed(["Semantic search speed for knowledge base retrieval"]);
const far = await embedder.embed(["Recipe for sourdough bread with starter"]);
console.log(`cos(query, related) = ${dot(q, near[0]).toFixed(4)} | cos(query, unrelated) = ${dot(q, far[0]).toFixed(4)}`);
