# Durable Scheduler Overlap Policy

Scheduler overlap is evaluated per Scheduled Job id, not per gateway lane or Session, so scheduler-produced inbound queue rows carry the Scheduled Job id as durable identity. An active Scheduled Firing is a pending inbound row or a fresh processing row for that job; dead-lettered, completed, and stale processing rows do not block overlap decisions. `skip` suppresses a new firing when any active firing exists, `enqueue` always appends, and `replace` coalesces pending firings by keeping only the newest pending row behind any processing turn without cancelling the active Turn.

This deliberately makes `replace` a backlog coalescing policy rather than a cancellation policy. Cancelling a processing Turn from scheduler overlap would couple scheduler policy to daemon turn cancellation and interrupt tool/provider work, while the chosen policy still prevents unbounded backlog for jobs that only need the newest missed firing.
