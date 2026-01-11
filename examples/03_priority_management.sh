#!/usr/bin/env bash
# Example 03: Priority Management
#
# Demonstrates using priorities to control task execution order.

set -e

echo "=== Neuraphage Priority Management ==="
echo

# Priority levels:
#   0 = Critical (highest)
#   1 = High
#   2 = Medium (default)
#   3 = Low
#   4 = Lowest

echo "Creating tasks with different priorities..."

# Critical priority (0)
neuraphage new "Fix security vulnerability" -p 0 -t security -t critical

# High priority (1)
neuraphage new "Complete sprint deliverable" -p 1 -t sprint

# Medium priority (2 - default)
neuraphage new "Refactor legacy code" -t refactor

# Low priority (3)
neuraphage new "Update development docs" -p 3 -t docs

# Lowest priority (4)
neuraphage new "Research new framework" -p 4 -t research

echo
echo "All tasks (note priority column):"
neuraphage list --all

echo
echo "Statistics:"
neuraphage stats

echo
echo "Done! The scheduler will process tasks by priority (0 first, then 1, etc.)"
