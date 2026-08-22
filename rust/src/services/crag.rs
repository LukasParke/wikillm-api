//! Corrective Retrieval Augmented Generation: grade retrieval quality and
//! trigger corrective actions when evidence is insufficient.

use crate::error::Result;
use crate::llm::provider::DynLlmProvider;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RetrievalGrade {
    Correct,
    Ambiguous,
    Incorrect,
}

#[derive(Debug, Clone, Serialize)]
pub struct CragResult {
    pub grade: RetrievalGrade,
    pub rewritten_query: Option<String>,
}

const GRADE_SYSTEM: &str = "You are a retrieval quality assessor. Given a question and retrieved passages, grade whether the passages contain sufficient information to answer the question. Respond ONLY with JSON: {\"grade\":\"correct\"|\"ambiguous\"|\"incorrect\",\"rewritten_query\":null|\"better search query\"}. Grade 'correct' if passages clearly answer the question, 'ambiguous' if partially relevant, 'incorrect' if irrelevant.";

const REWRITE_SYSTEM: &str = "Rewrite this search query to be more effective for retrieving relevant documents. Respond ONLY with the rewritten query text.";

pub async fn grade_retrieval(
    llm: &DynLlmProvider,
    query: &str,
    result_snippets: &[String],
) -> Result<CragResult> {
    let evidence = result_snippets
        .iter()
        .enumerate()
        .map(|(i, s)| format!("[{}] {}", i + 1, s))
        .collect::<Vec<_>>()
        .join("\n\n");

    let user_prompt = format!("Question: {query}\n\nPassages:\n{evidence}");
    let messages: Vec<(&str, &str)> = vec![
        ("system", GRADE_SYSTEM),
        ("user", &user_prompt),
    ];
    let raw = llm.chat(&messages, 0.0, 200).await?;

    #[derive(Deserialize)]
    struct GradeResponse {
        grade: String,
        #[serde(default)]
        rewritten_query: Option<String>,
    }

    let start = raw.find('{');
    let end = raw.rfind('}');
    if let (Some(s), Some(e)) = (start, end) {
        if let Ok(parsed) = serde_json::from_str::<GradeResponse>(&raw[s..=e]) {
            let grade = match parsed.grade.as_str() {
                "correct" => RetrievalGrade::Correct,
                "incorrect" => RetrievalGrade::Incorrect,
                _ => RetrievalGrade::Ambiguous,
            };
            return Ok(CragResult {
                grade,
                rewritten_query: parsed.rewritten_query,
            });
        }
    }
    // Default to ambiguous if we can't parse
    Ok(CragResult { grade: RetrievalGrade::Ambiguous, rewritten_query: None })
}

pub async fn rewrite_query(llm: &DynLlmProvider, original: &str) -> Result<String> {
    let messages: Vec<(&str, &str)> = vec![
        ("system", REWRITE_SYSTEM),
        ("user", original),
    ];
    llm.chat(&messages, 0.0, 200).await
}
