# Gateway — Context

Bridge to external chat platforms. Owns Discord/Telegram/loopback connectors, inbound + outbound queues (durable SQLite), scheduler, lane routing, per-lane long-lived agents with idle eviction, render registry.

## Glossary

**Scheduled Job**:
A configured gateway schedule that fires one prompt into one platform lane on a cron cadence.
_Avoid_: cron task, scheduler row, background job

**Scheduled Firing**:
One due occurrence of a **Scheduled Job** that is accepted, skipped, or replaced according to that job's overlap policy.
_Avoid_: run, tick, cron event

**Active Scheduled Firing**:
A **Scheduled Firing** whose inbound queue row is still pending or freshly processing.
_Avoid_: running flag, live cron

## Relationships

- A **Scheduled Job** owns its overlap policy; overlap is evaluated against prior **Scheduled Firings** of the same job id, not against all work in the same lane or **Session**.
- A **Scheduled Firing** produces at most one inbound queue row.
- A scheduler-produced inbound queue row carries the **Scheduled Job** id so durable overlap checks do not infer identity from lane, user id, or prompt text.
- A pending inbound row always counts as an **Active Scheduled Firing**.
- A processing inbound row counts as an **Active Scheduled Firing** only while its heartbeat is fresh; stale processing rows should be recovered or ignored for overlap decisions.
- Dead-lettered and completed inbound rows do not count as **Active Scheduled Firings**.
- A scheduler overlap check should recover stale processing rows for that **Scheduled Job** before applying the job's overlap policy.
- The `skip` overlap policy suppresses a new **Scheduled Firing** when any **Active Scheduled Firing** exists for the same **Scheduled Job**.
- The `enqueue` overlap policy always accepts a new **Scheduled Firing**, even when other active firings for the same **Scheduled Job** exist.
- The `replace` overlap policy coalesces pending **Scheduled Firings** for the same **Scheduled Job**; it must not cancel a processing **Turn**, and it should keep exactly the newest pending firing behind any active processing row.
- Scheduler fire counts track every due cron occurrence, including skipped, enqueued, enqueue-failed, and replacement occurrences.
- Replaced firings are counted separately from skipped firings because `replace` accepts the newest firing while suppressing older pending ones.

## See also

- ADR-0001 (daemon-required frontends).
- `src/daemon/CONTEXT.md` — gateway shares daemon client transport.
