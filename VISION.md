# Hugr Vision

Hugr is a project memory and intelligence system for agents. It gives them a durable, structured understanding of a project: what the code does, what changed, what was learned, what failed before, what decisions still matter, and what context is needed for the next task.

## Product Principle

Hugr does not just store memories. It decides what an agent needs to know right now.

The flagship primitive is:

```bash
hugr context "task"
```

Everything else in the product — memory, code graph, sessions, sync — exists to make that one command compile better context: relevant, cited, budgeted, and honest about staleness and risk.

## Deployment Stance

Hugr is deployment-flexible: local for private repos and personal agents, cloud for hosted agents and always-on memory, hybrid when code intelligence should run near the repository while memory runs elsewhere. The same concepts, schema, and commands work in every mode, and the core developer experience never requires the cloud.

## Product Test

Hugr is working when this command is obviously useful inside a real project:

```bash
hugr context "make this change"
```

The output should help an agent act with fewer files opened, fewer wrong assumptions, fewer repeated mistakes, and better tests.

## Status

All capabilities in this vision have a first shipped implementation. See the [README](README.md) for the shipped surface, [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for how it is built, and [docs/ROADMAP.md](docs/ROADMAP.md) for what comes next.
