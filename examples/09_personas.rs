//! Example 09: Personas
//!
//! Demonstrates the persona system for assigning AI roles to tasks.
//!
//! Run with: cargo run --example 09_personas

use neuraphage::personas::{ModelTier, Persona, PersonaStore};

fn main() -> neuraphage::Result<()> {
    println!("=== Personas Example ===\n");

    // Create a persona store (comes with defaults)
    let store = PersonaStore::new();

    println!("Default personas:");
    for persona in store.all() {
        println!(
            "  - {} ({:?}): {}",
            persona.name, persona.model_tier, persona.description
        );
        if !persona.auto_assign.is_empty() {
            println!("    Auto-assigns to: {:?}", persona.auto_assign);
        }
    }

    println!("\n--- Custom Personas ---\n");

    // Create custom personas
    let mut custom_store = PersonaStore::new();

    custom_store.add(
        Persona::new(
            "security-expert",
            "Security Expert",
            "Specializes in security audits and vulnerability analysis",
        )
        .with_tier(ModelTier::Expert)
        .with_prompt(
            "You are a security expert. Focus on identifying vulnerabilities, \
             reviewing authentication flows, and ensuring secure coding practices.",
        )
        .with_auto_assign(vec!["security".to_string(), "auth".to_string()]),
    );

    custom_store.add(
        Persona::new(
            "db-specialist",
            "Database Specialist",
            "Expert in database design and optimization",
        )
        .with_tier(ModelTier::Worker)
        .with_prompt(
            "You are a database specialist. Focus on schema design, \
             query optimization, and data integrity.",
        )
        .with_auto_assign(vec!["database".to_string(), "sql".to_string()]),
    );

    // Set escalation chain
    custom_store.set_escalation_chain(vec![
        "mid".to_string(),
        "senior".to_string(),
        "security-expert".to_string(),
    ]);

    // Find persona for a category
    println!("Looking up persona for 'security' category:");
    if let Some(persona) = custom_store.for_category("security") {
        println!("  Found: {} ({:?})", persona.name, persona.model_tier);
    }

    // Get default persona
    println!("\nDefault persona:");
    if let Some(default) = custom_store.default_persona() {
        println!("  {}: {}", default.name, default.description);
    }

    // Demonstrate escalation
    println!("\nEscalation chain:");
    let chain = custom_store.escalation_chain();
    for (i, id) in chain.iter().enumerate() {
        if let Some(p) = custom_store.get(id) {
            println!("  {}. {} ({:?})", i + 1, p.name, p.model_tier);
        }
    }

    // Test escalation
    println!("\nEscalating from 'mid':");
    if let Some(next) = custom_store.escalate("mid") {
        println!("  Next: {}", next.name);
    }

    println!("\nDone!");
    Ok(())
}
