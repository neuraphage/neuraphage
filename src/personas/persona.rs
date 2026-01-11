//! Persona definitions and management.
//!
//! Personas provide specialized behaviors and expertise levels for tasks.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Model tier for cost/capability routing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelTier {
    /// Fast, cheap model for routing and classification.
    Router,
    /// Standard model for most tasks.
    #[default]
    Worker,
    /// Expensive, powerful model for complex reasoning.
    Expert,
}

/// A persona definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Persona {
    /// Unique identifier for the persona.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Brief description.
    pub description: String,
    /// Model tier to use.
    #[serde(default)]
    pub model_tier: ModelTier,
    /// System prompt for this persona.
    pub system_prompt: String,
    /// Categories this persona handles (for auto-routing).
    #[serde(default)]
    pub auto_assign: Vec<String>,
    /// Whether this is an internal persona (not user-assignable).
    #[serde(default)]
    pub internal: bool,
    /// Whether this persona is enabled.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

impl Persona {
    /// Create a new persona.
    pub fn new(id: impl Into<String>, name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            model_tier: ModelTier::default(),
            system_prompt: String::new(),
            auto_assign: Vec::new(),
            internal: false,
            enabled: true,
        }
    }

    /// Set the model tier.
    pub fn with_tier(mut self, tier: ModelTier) -> Self {
        self.model_tier = tier;
        self
    }

    /// Set the system prompt.
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = prompt.into();
        self
    }

    /// Add auto-assign categories.
    pub fn with_auto_assign(mut self, categories: Vec<String>) -> Self {
        self.auto_assign = categories;
        self
    }

    /// Mark as internal persona.
    pub fn internal(mut self) -> Self {
        self.internal = true;
        self
    }

    /// Check if persona handles a category.
    pub fn handles(&self, category: &str) -> bool {
        self.auto_assign.iter().any(|c| c == category)
    }
}

/// Store for managing personas.
pub struct PersonaStore {
    /// Personas by ID.
    personas: HashMap<String, Persona>,
    /// Default persona ID.
    default_persona: String,
    /// Escalation chain (from junior to senior).
    escalation_chain: Vec<String>,
}

impl PersonaStore {
    /// Create a new persona store with default personas.
    pub fn new() -> Self {
        let mut store = Self {
            personas: HashMap::new(),
            default_persona: "mid".to_string(),
            escalation_chain: vec![
                "junior".to_string(),
                "mid".to_string(),
                "senior".to_string(),
                "architect".to_string(),
            ],
        };
        store.load_defaults();
        store
    }

    /// Load default personas.
    fn load_defaults(&mut self) {
        // Senior engineer
        self.add(
            Persona::new("senior", "Senior", "Experienced engineer, concise and direct")
                .with_tier(ModelTier::Expert)
                .with_prompt(
                    "You are a senior software engineer with 15+ years of experience.\n\n\
                     Traits:\n\
                     - Direct and concise - no fluff, get to the point\n\
                     - Opinionated but open to discussion when challenged\n\
                     - Assumes competence - doesn't over-explain basics\n\
                     - Focuses on architecture, edge cases, and maintainability\n\
                     - Will push back on bad ideas\n\n\
                     Communication style:\n\
                     - Short sentences, minimal filler\n\
                     - Uses technical terminology without explanation\n\
                     - Points out potential issues proactively\n\
                     - Suggests alternatives when declining an approach",
                )
                .with_auto_assign(vec![
                    "complex-reasoning".to_string(),
                    "architecture".to_string(),
                    "code-review".to_string(),
                    "refactoring".to_string(),
                ]),
        );

        // Mid-level engineer
        self.add(
            Persona::new("mid", "Mid", "Solid engineer, explains reasoning clearly")
                .with_tier(ModelTier::Worker)
                .with_prompt(
                    "You are a mid-level software engineer with 5-7 years of experience.\n\n\
                     Traits:\n\
                     - Balanced between speed and thoroughness\n\
                     - Explains reasoning but doesn't belabor points\n\
                     - Asks clarifying questions when requirements are ambiguous\n\
                     - Good at breaking down problems into steps\n\
                     - Collaborative, open to feedback\n\n\
                     Communication style:\n\
                     - Clear and structured\n\
                     - Brief explanations of \"why\" alongside \"what\"\n\
                     - Uses examples when helpful\n\
                     - Acknowledges trade-offs",
                )
                .with_auto_assign(vec![
                    "standard-tasks".to_string(),
                    "code-generation".to_string(),
                    "bug-fixes".to_string(),
                    "feature-implementation".to_string(),
                ]),
        );

        // Junior engineer
        self.add(
            Persona::new(
                "junior",
                "Junior",
                "Eager learner, thorough explanations, asks questions",
            )
            .with_tier(ModelTier::Worker)
            .with_prompt(
                "You are a junior software engineer with 1-2 years of experience.\n\n\
                     Traits:\n\
                     - Eager and thorough\n\
                     - Explains thinking step-by-step\n\
                     - Asks clarifying questions frequently\n\
                     - Double-checks assumptions\n\
                     - Cautious about making changes without confirmation\n\n\
                     Communication style:\n\
                     - Verbose, shows work\n\
                     - Explains terminology when using it\n\
                     - Asks \"did I understand correctly?\" style questions\n\
                     - Celebrates small wins",
            )
            .with_auto_assign(vec![
                "documentation".to_string(),
                "simple-tasks".to_string(),
                "learning-exploration".to_string(),
            ]),
        );

        // Reviewer
        self.add(
            Persona::new("reviewer", "Reviewer", "Critical eye for code review and security")
                .with_tier(ModelTier::Expert)
                .with_prompt(
                    "You are a staff engineer specializing in code review and security.\n\n\
                     Traits:\n\
                     - Skeptical by default - looks for problems\n\
                     - Security-minded - thinks about attack vectors\n\
                     - Performance-aware - spots inefficiencies\n\
                     - Standards-focused - enforces consistency\n\
                     - Constructive - criticism comes with suggestions\n\n\
                     Communication style:\n\
                     - Structured feedback (issues, suggestions, questions)\n\
                     - References specific lines/patterns\n\
                     - Rates severity (critical, important, nitpick)\n\
                     - Explains the \"why\" behind concerns",
                )
                .with_auto_assign(vec![
                    "code-review".to_string(),
                    "security-audit".to_string(),
                    "pr-review".to_string(),
                ]),
        );

        // Architect
        self.add(
            Persona::new(
                "architect",
                "Architect",
                "Systems thinker, designs for scale and maintainability",
            )
            .with_tier(ModelTier::Expert)
            .with_prompt(
                "You are a software architect with expertise in system design.\n\n\
                     Traits:\n\
                     - Thinks in systems, not just code\n\
                     - Considers scale, maintainability, and evolution\n\
                     - Balances ideal solutions with pragmatic constraints\n\
                     - Documents decisions and rationale\n\
                     - Identifies dependencies and risks\n\n\
                     Communication style:\n\
                     - Uses diagrams and visual representations\n\
                     - Presents options with trade-offs\n\
                     - References patterns and prior art\n\
                     - Thinks out loud about constraints",
            )
            .with_auto_assign(vec![
                "architecture".to_string(),
                "system-design".to_string(),
                "technical-planning".to_string(),
            ]),
        );

        // Router (internal)
        self.add(
            Persona::new("router", "Router", "Fast classifier for task routing")
                .with_tier(ModelTier::Router)
                .with_prompt(
                    "You are a task router. Your job is to quickly classify tasks.\n\n\
                     Respond ONLY with a JSON object:\n\
                     {\n\
                       \"category\": \"<category>\",\n\
                       \"complexity\": \"low|medium|high\",\n\
                       \"suggested-persona\": \"<persona-name>\",\n\
                       \"tags\": [\"<tag1>\", \"<tag2>\"]\n\
                     }\n\n\
                     Do not explain. Do not elaborate. JSON only.",
                )
                .internal(),
        );
    }

    /// Add a persona.
    pub fn add(&mut self, persona: Persona) {
        self.personas.insert(persona.id.clone(), persona);
    }

    /// Get a persona by ID.
    pub fn get(&self, id: &str) -> Option<&Persona> {
        self.personas.get(id)
    }

    /// Get the default persona.
    pub fn default_persona(&self) -> Option<&Persona> {
        self.get(&self.default_persona)
    }

    /// Set the default persona.
    pub fn set_default(&mut self, id: impl Into<String>) {
        self.default_persona = id.into();
    }

    /// Get all enabled, user-assignable personas.
    pub fn available(&self) -> Vec<&Persona> {
        self.personas.values().filter(|p| p.enabled && !p.internal).collect()
    }

    /// Get all personas (including internal).
    pub fn all(&self) -> Vec<&Persona> {
        self.personas.values().collect()
    }

    /// Find the best persona for a category.
    pub fn for_category(&self, category: &str) -> Option<&Persona> {
        self.personas.values().find(|p| p.enabled && p.handles(category))
    }

    /// Get the next persona in the escalation chain.
    pub fn escalate(&self, current: &str) -> Option<&Persona> {
        let pos = self.escalation_chain.iter().position(|id| id == current)?;
        let next_id = self.escalation_chain.get(pos + 1)?;
        self.get(next_id).filter(|p| p.enabled)
    }

    /// Load personas from a YAML file.
    pub fn load_from_file(&mut self, path: &Path) -> Result<()> {
        let content = std::fs::read_to_string(path)?;
        let personas: Vec<Persona> = serde_yaml::from_str(&content)?;
        for persona in personas {
            self.add(persona);
        }
        Ok(())
    }

    /// Get the escalation chain.
    pub fn escalation_chain(&self) -> &[String] {
        &self.escalation_chain
    }

    /// Set the escalation chain.
    pub fn set_escalation_chain(&mut self, chain: Vec<String>) {
        self.escalation_chain = chain;
    }
}

impl Default for PersonaStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_persona_creation() {
        let persona = Persona::new("test", "Test", "A test persona");
        assert_eq!(persona.id, "test");
        assert_eq!(persona.name, "Test");
        assert_eq!(persona.model_tier, ModelTier::Worker);
        assert!(persona.enabled);
        assert!(!persona.internal);
    }

    #[test]
    fn test_persona_builder() {
        let persona = Persona::new("senior", "Senior", "Senior engineer")
            .with_tier(ModelTier::Expert)
            .with_prompt("You are a senior engineer.")
            .with_auto_assign(vec!["architecture".to_string()])
            .internal();

        assert_eq!(persona.model_tier, ModelTier::Expert);
        assert_eq!(persona.system_prompt, "You are a senior engineer.");
        assert!(persona.handles("architecture"));
        assert!(!persona.handles("documentation"));
        assert!(persona.internal);
    }

    #[test]
    fn test_persona_store_defaults() {
        let store = PersonaStore::new();

        assert!(store.get("senior").is_some());
        assert!(store.get("mid").is_some());
        assert!(store.get("junior").is_some());
        assert!(store.get("reviewer").is_some());
        assert!(store.get("architect").is_some());
        assert!(store.get("router").is_some());
    }

    #[test]
    fn test_persona_store_default_persona() {
        let store = PersonaStore::new();
        let default = store.default_persona().unwrap();
        assert_eq!(default.id, "mid");
    }

    #[test]
    fn test_persona_store_available() {
        let store = PersonaStore::new();
        let available = store.available();

        // Router is internal, so should not be in available
        assert!(!available.iter().any(|p| p.id == "router"));
        assert!(available.iter().any(|p| p.id == "senior"));
    }

    #[test]
    fn test_persona_store_for_category() {
        let store = PersonaStore::new();

        let persona = store.for_category("architecture").unwrap();
        assert!(persona.id == "senior" || persona.id == "architect");

        let persona = store.for_category("documentation").unwrap();
        assert_eq!(persona.id, "junior");
    }

    #[test]
    fn test_persona_store_escalation() {
        let store = PersonaStore::new();

        let next = store.escalate("junior").unwrap();
        assert_eq!(next.id, "mid");

        let next = store.escalate("mid").unwrap();
        assert_eq!(next.id, "senior");

        let next = store.escalate("senior").unwrap();
        assert_eq!(next.id, "architect");

        assert!(store.escalate("architect").is_none());
    }

    #[test]
    fn test_model_tier_default() {
        assert_eq!(ModelTier::default(), ModelTier::Worker);
    }

    #[test]
    fn test_persona_store_add_custom() {
        let mut store = PersonaStore::new();
        store.add(Persona::new("chaos", "Chaos Gremlin", "Chaos engineering specialist"));

        assert!(store.get("chaos").is_some());
    }
}
