#!/usr/bin/env bash
# Example 02: Task Dependencies
#
# Demonstrates creating task dependencies so tasks execute in order.

set -e

echo "=== Neuraphage Task Dependencies ==="
echo

# Create a workflow with dependencies
echo "Creating task workflow..."

# Design phase
DESIGN=$(neuraphage new "Design database schema" -p 1 -t design | grep -oE 'eg-[a-f0-9]+')
echo "Created design task: $DESIGN"

# Implementation depends on design
IMPL=$(neuraphage new "Implement database models" -p 2 -t impl | grep -oE 'eg-[a-f0-9]+')
echo "Created impl task: $IMPL"

# Testing depends on implementation
TEST=$(neuraphage new "Write database integration tests" -p 2 -t testing | grep -oE 'eg-[a-f0-9]+')
echo "Created test task: $TEST"

# Set up dependencies
echo
echo "Setting up dependencies..."
neuraphage depend "$IMPL" "$DESIGN"  # impl blocked by design
neuraphage depend "$TEST" "$IMPL"    # test blocked by impl

# Show blocked tasks
echo
echo "Blocked tasks:"
neuraphage blocked

# Show ready tasks (only design should be ready)
echo
echo "Ready tasks (only design should be ready):"
neuraphage ready

# Complete design task
echo
echo "Completing design task..."
neuraphage close "$DESIGN" -s completed

# Now impl should be ready
echo
echo "Ready tasks (impl should now be ready):"
neuraphage ready

echo
echo "Done!"
