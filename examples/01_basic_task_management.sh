#!/usr/bin/env bash
# Example 01: Basic Task Management
#
# Demonstrates creating, listing, and managing tasks with np.

set -e

echo "=== Neuraphage Basic Task Management ==="
echo

# Check if daemon is running
if ! np ping > /dev/null 2>&1; then
    echo "Starting daemon..."
    np daemon --foreground &
    DAEMON_PID=$!
    sleep 1
    trap "kill $DAEMON_PID 2>/dev/null" EXIT
fi

# Create some tasks
echo "Creating tasks..."
np new "Implement user authentication" -p 1 -t auth -t security
np new "Write unit tests for auth module" -p 2 -t auth -t testing
np new "Update API documentation" -p 3 -t docs

# List all open tasks
echo
echo "Open tasks:"
np list

# Show task statistics
echo
echo "Statistics:"
np stats

# Show ready tasks (no blockers)
echo
echo "Ready tasks:"
np ready

echo
echo "Done!"
