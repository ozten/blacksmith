#!/bin/bash
# ralph-wiggums-loop.sh — autonomous agent harness

MAX_ITERATIONS=${1:-25}
PROMPT_FILE="PROMPT.md"
PROGRESS_FILE="PROGRESS.txt"
COUNTER_FILE=".iteration_counter"
BACKOFF_SECONDS=2          # Initial delay between iterations
MAX_BACKOFF_SECONDS=600    # Max 10 minutes between retries
CONSECUTIVE_RATE_LIMITS=0  # Track consecutive rate-limited sessions
STALE_TIMEOUT_MINUTES=20   # Kill session if no output growth for this long
MAX_EMPTY_RETRIES=2        # Retry limit for empty/crashed sessions
STOP_FILE="STOP"           # Touch this file to gracefully stop after current iteration

# Load persistent counter (starts at 0 if file doesn't exist)
if [ -f "$COUNTER_FILE" ]; then
    global_iteration=$(cat "$COUNTER_FILE")
else
    global_iteration=0
fi

check_rate_limited() {
    local output_file="$1"
    # Only check the final "type":"result" event for rate limit signals.
    # Previous approach grepped the entire JSONL (including tool results containing
    # source code), causing false positives when the agent read files mentioning
    # rate limiting.
    local result_line
    result_line=$(grep '"type":"result"' "$output_file" 2>/dev/null | tail -1)
    [ -z "$result_line" ] && return 1  # no result event = not rate limited

    # 1. Check if the session ended with an error
    if echo "$result_line" | grep -q '"is_error":true'; then
        # Check for rate limit in the error result
        if echo "$result_line" | grep -qi 'rate.limit\|rate_limit\|usage.limit\|hit your limit'; then
            return 0  # rate limited
        fi
    fi

    # 2. Check for a non-success subtype indicating rate limiting
    if echo "$result_line" | grep -q '"subtype":"error"'; then
        if echo "$result_line" | grep -qi 'rate.limit\|rate_limit'; then
            return 0  # rate limited
        fi
    fi

    return 1  # not rate limited
}

# run_with_watchdog <prompt> <output_file>
# Runs claude in a background subshell, monitors output growth.
# Kills the session if no new output for STALE_TIMEOUT_MINUTES.
# Returns 124 on timeout (matching GNU timeout convention), else claude's exit code.
run_with_watchdog() {
    local prompt="$1"
    local output_file="$2"
    local stale_limit=$((STALE_TIMEOUT_MINUTES * 60))  # convert to seconds
    local check_interval=60  # check every 60 seconds

    # Run claude in a background subshell
    (
        claude -p "$prompt" --dangerously-skip-permissions \
            --verbose --output-format stream-json \
            2>&1 | tee "$output_file"
    ) &
    local bg_pid=$!

    local last_size=0
    local stale_seconds=0

    # Monitor loop: check output growth
    while kill -0 "$bg_pid" 2>/dev/null; do
        sleep "$check_interval"

        # Check if process is still running after sleep
        if ! kill -0 "$bg_pid" 2>/dev/null; then
            break
        fi

        local current_size
        current_size=$(wc -c < "$output_file" 2>/dev/null || echo 0)

        if [ "$current_size" -gt "$last_size" ]; then
            # Output is growing — reset stale timer
            last_size=$current_size
            stale_seconds=0
        else
            # No growth
            stale_seconds=$((stale_seconds + check_interval))
            echo "  ⏳ No output growth for ${stale_seconds}s (limit: ${stale_limit}s)"

            if [ "$stale_seconds" -ge "$stale_limit" ]; then
                echo "  ✗ Session stale for ${STALE_TIMEOUT_MINUTES}m — killing"
                # Kill child processes (claude, tee) then the subshell
                pkill -P "$bg_pid" 2>/dev/null
                kill "$bg_pid" 2>/dev/null
                wait "$bg_pid" 2>/dev/null
                return 124
            fi
        fi
    done

    # Process finished naturally — get its exit code
    wait "$bg_pid"
    return $?
}

iteration=0
current_backoff=$BACKOFF_SECONDS

while [ $iteration -lt $MAX_ITERATIONS ]; do
    # Check for STOP file — graceful shutdown
    if [ -f "$STOP_FILE" ]; then
        echo "⛔ STOP file detected. Shutting down gracefully."
        rm -f "$STOP_FILE"
        break
    fi

    echo "═══════════════════════════════════════════"
    echo "Iteration $((iteration + 1)) of $MAX_ITERATIONS (global: $global_iteration)"
    echo "═══════════════════════════════════════════"

    # R2: Reset zombie IN_PROGRESS beads from failed prior sessions
    ZOMBIES=$(bd list --status=in_progress --json 2>/dev/null | jq -r '.[].id' 2>/dev/null)
    if [ -n "$ZOMBIES" ]; then
        ZOMBIE_COUNT=$(echo "$ZOMBIES" | wc -l | tr -d ' ')
        echo "♻ Resetting $ZOMBIE_COUNT zombie IN_PROGRESS beads to open..."
        echo "$ZOMBIES" | while read -r zid; do
            # Strip project prefix if present (e.g., project-abc1 -> abc1)
            short_id="${zid##*-}"
            bd update "$short_id" --status=open 2>/dev/null
            echo "  ↺ $zid → open"
        done
    fi

    # Check for open tasks in beads
    OPEN_TASKS=$(bd ready --json 2>/dev/null | jq -r 'length')
    if [ "$OPEN_TASKS" -eq 0 ] || [ -z "$OPEN_TASKS" ]; then
        echo "No open tasks remaining. Exiting."
        break
    fi
    echo "Open tasks: $OPEN_TASKS"

    # ── Pre-session: inject performance feedback from last session ──
    PERF_BRIEF=$(tools/self-improvement brief --last 5 2>/dev/null)
    PROMPT_CONTENT="$(cat $PROMPT_FILE)"
    if [ -n "$PERF_BRIEF" ]; then
        PROMPT_CONTENT="${PERF_BRIEF}

---

${PROMPT_CONTENT}"
    fi

    # ── Inner retry loop for empty/crashed sessions ──
    session_ok=false
    for retry in $(seq 0 $MAX_EMPTY_RETRIES); do
        OUTPUT_FILE="./claude-iteration-$global_iteration.jsonl"

        if [ "$retry" -gt 0 ]; then
            echo "  ⟳ Retry $retry/$MAX_EMPTY_RETRIES (previous session produced <100 bytes)"
        fi

        run_with_watchdog "$PROMPT_CONTENT" "$OUTPUT_FILE"
        EXIT_CODE=$?

        if [ $EXIT_CODE -eq 124 ]; then
            echo "  ✗ Session killed by watchdog (stale for ${STALE_TIMEOUT_MINUTES}m)"
        elif [ $EXIT_CODE -ne 0 ]; then
            echo "  Claude exited with error code $EXIT_CODE"
        fi

        # Check output size
        OUTPUT_SIZE=$(wc -c < "$OUTPUT_FILE" 2>/dev/null || echo 0)
        if [ "$OUTPUT_SIZE" -ge 100 ]; then
            session_ok=true
            break
        fi

        echo "  ⚠ Empty session: $OUTPUT_SIZE bytes (threshold: 100)"

        # Bump global_iteration for a fresh filename on retry
        if [ "$retry" -lt "$MAX_EMPTY_RETRIES" ]; then
            ((global_iteration++))
            echo "$global_iteration" > "$COUNTER_FILE"
        fi
    done

    # ── Post-session logging (only if meaningful output) ──
    if [ "$session_ok" = true ] && [ -f "$OUTPUT_FILE" ]; then
        tools/self-improvement log "$OUTPUT_FILE"
    else
        echo "  ⚠ Skipping metrics log — no meaningful output after $((MAX_EMPTY_RETRIES + 1)) attempts"
    fi

    # ── Periodic deep analysis every 10 iterations ──
    if [ $((global_iteration % 10)) -eq 0 ] && [ $global_iteration -gt 0 ]; then
        echo "── Periodic analysis (every 10 iterations) ──"
        tools/self-improvement analyze --last 10
    fi

    # Check for rate limiting with exponential backoff
    if [ "$session_ok" = true ] && check_rate_limited "$OUTPUT_FILE"; then
        ((CONSECUTIVE_RATE_LIMITS++))
        # Exponential backoff: 2, 4, 8, 16, ... capped at MAX_BACKOFF_SECONDS
        current_backoff=$((BACKOFF_SECONDS * (2 ** CONSECUTIVE_RATE_LIMITS)))
        if [ $current_backoff -gt $MAX_BACKOFF_SECONDS ]; then
            current_backoff=$MAX_BACKOFF_SECONDS
        fi
        echo "⚠ Rate limited (${CONSECUTIVE_RATE_LIMITS}x consecutive). Backing off ${current_backoff}s..."
        sleep $current_backoff

        # After 5 consecutive rate limits, exit — the window hasn't reset
        if [ $CONSECUTIVE_RATE_LIMITS -ge 5 ]; then
            echo "✗ 5 consecutive rate limits. Exiting — try again after the reset window."
            break
        fi

        # Don't increment iteration counter for rate-limited sessions
        ((global_iteration++))
        echo "$global_iteration" > "$COUNTER_FILE"
        continue
    fi

    # Successful session — reset backoff
    CONSECUTIVE_RATE_LIMITS=0
    current_backoff=$BACKOFF_SECONDS

    # ── Post-session commit check (R1/R13) ──
    # Only bd-finish counts as committed (bd close alone may be closing sub-beads)
    if [ "$session_ok" = true ]; then
        if grep -q "bd-finish" "$OUTPUT_FILE" 2>/dev/null; then
            echo "  ✓ Session committed work (bd-finish detected)"
        else
            echo "  ⚠ Session did NOT commit (no bd-finish detected) — possible cutoff"
            # Any beads left in_progress will be cleaned up at the top of the next iteration
        fi
    fi

    # Check for explicit usage limit message
    if [ "$session_ok" = true ] && grep -qi "usage limit reached" "$OUTPUT_FILE"; then
        echo "Usage limit reached. Exiting."
        break
    fi

    ((iteration++))
    ((global_iteration++))

    # Persist the counter
    echo "$global_iteration" > "$COUNTER_FILE"

    # Brief pause between successful iterations
    sleep $BACKOFF_SECONDS
done

echo "Loop completed: $iteration productive iterations, $CONSECUTIVE_RATE_LIMITS trailing rate limits (global counter: $global_iteration)"
