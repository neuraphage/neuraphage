#!/usr/bin/env bash
# Example 01: Basic Task Management
#
# Demonstrates creating, listing, and managing tasks with neuraphage.

set -e

echo "=== Neuraphage Basic Task Management ==="
echo

# Check if daemon is running
if ! neuraphage ping > /dev/null 2>&1; then
    echo "Starting daemon..."
    neuraphage daemon --foreground &
    DAEMON_PID=$!
    sleep 1
    trap "kill $DAEMON_PID 2>/dev/null" EXIT
fi

# Create some tasks
echo "Creating tasks..."
neuraphage new "Implement user authentication" -p 1 -t auth -t security
neuraphage new "Write unit tests for auth module" -p 2 -t auth -t testing
neuraphage new "Update API documentation" -p 3 -t docs

# List all open tasks
echo
echo "Open tasks:"
neuraphage list

# Show task statistics
echo
echo "Statistics:"
neuraphage stats

# Show ready tasks (no blockers)
echo
echo "Ready tasks:"
neuraphage ready

echo
echo "Done!"
