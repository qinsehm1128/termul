import type { ConversationId, ExecutionTarget } from './conversation.types'

export type ScheduledTaskId = string & { readonly __brand: 'ScheduledTaskId' }
export type ScheduledTaskRunId = string & { readonly __brand: 'ScheduledTaskRunId' }

export type ScheduledTaskStatus = 'draft' | 'active' | 'paused'
export type ScheduledTaskRunStatus =
  | 'queued'
  | 'running'
  | 'succeeded'
  | 'failed'
  | 'cancelled'
  | 'skipped'
  | 'interrupted'
export type ScheduledTaskRunTrigger = 'scheduled' | 'manual' | 'catchUp' | 'retry'
export type ScheduledTaskOverlapPolicy = 'skip' | 'bufferOne'
export type ScheduledTaskCatchUpPolicy = 'skip' | 'latestOnce'

export type ScheduledTaskSchedule =
  | { kind: 'cron'; expression: string; timezone: string }
  | { kind: 'interval'; everySeconds: number; anchorAt: string }
  | { kind: 'at'; at: string }

export interface ScheduledTaskExecutionPolicy {
  overlap: ScheduledTaskOverlapPolicy
  catchUp: ScheduledTaskCatchUpPolicy
  catchUpWindowSeconds: number
}

export interface ScheduledTaskRecordV1 {
  schemaVersion: 1
  taskId: ScheduledTaskId
  projectId: string
  name: string
  description: string
  status: ScheduledTaskStatus
  schedule: ScheduledTaskSchedule
  executionPolicy: ScheduledTaskExecutionPolicy
  prompt: string
  agentConfigId: string
  executionTarget: ExecutionTarget
  executionCwd: string
  workspaceCwd: string
  sourceConversationId: ConversationId | null
  permissions: string[]
  skillTemplateVersion: number
  revision: number
  draftHash: string
  createdAt: string
  updatedAt: string
  nextRunAt: string | null
}

export interface ScheduledTaskDraftInput {
  projectId: string
  name: string
  description: string
  schedule: ScheduledTaskSchedule
  executionPolicy?: Partial<ScheduledTaskExecutionPolicy>
  prompt: string
  agentConfigId: string
  executionTarget: ExecutionTarget
  executionCwd: string
  workspaceCwd: string
  sourceConversationId?: ConversationId | null
  permissions?: string[]
}

export interface ScheduledTaskSchedulePreview {
  normalized: ScheduledTaskSchedule
  nextRunTimes: string[]
}

export interface ScheduledTaskRunV1 {
  schemaVersion: 1
  runId: ScheduledTaskRunId
  taskId: ScheduledTaskId
  projectId: string
  trigger: ScheduledTaskRunTrigger
  status: ScheduledTaskRunStatus
  occurrenceKey: string
  scheduledFor: string
  queuedAt: string
  startedAt: string | null
  finishedAt: string | null
  conversationId: ConversationId | null
  taskSnapshotHash: string
  retryOfRunId: ScheduledTaskRunId | null
  summary: string | null
  errorCode: string | null
  errorDetail: string | null
  usage: unknown | null
}

export type ScheduledTaskAuditActor = 'human' | 'agent' | 'system'

export interface ScheduledTaskAuditEventV1 {
  schemaVersion: 1
  eventId: string
  taskId: ScheduledTaskId
  projectId: string
  action: string
  actor: ScheduledTaskAuditActor
  sourceConversationId: ConversationId | null
  sourceToolCallId: string | null
  beforeHash: string | null
  afterHash: string | null
  createdAt: string
}

export interface ScheduledTaskApi {
  previewSchedule(schedule: ScheduledTaskSchedule): Promise<ScheduledTaskSchedulePreview>
  listTasks(projectId?: string): Promise<ScheduledTaskRecordV1[]>
  getTask(taskId: ScheduledTaskId): Promise<ScheduledTaskRecordV1>
  createDraft(input: ScheduledTaskDraftInput): Promise<ScheduledTaskRecordV1>
  updateDraft(
    taskId: ScheduledTaskId,
    expectedRevision: number,
    input: ScheduledTaskDraftInput
  ): Promise<ScheduledTaskRecordV1>
  activateTask(
    taskId: ScheduledTaskId,
    expectedRevision: number,
    expectedDraftHash: string
  ): Promise<ScheduledTaskRecordV1>
  pauseTask(taskId: ScheduledTaskId, expectedRevision: number): Promise<ScheduledTaskRecordV1>
  resumeTask(taskId: ScheduledTaskId, expectedRevision: number): Promise<ScheduledTaskRecordV1>
  deleteTask(taskId: ScheduledTaskId, expectedRevision: number): Promise<void>
  runNow(taskId: ScheduledTaskId): Promise<ScheduledTaskRunV1>
  retryRun(taskId: ScheduledTaskId, runId: ScheduledTaskRunId): Promise<ScheduledTaskRunV1>
  listRuns(taskId: ScheduledTaskId): Promise<ScheduledTaskRunV1[]>
  listAudit(taskId: ScheduledTaskId): Promise<ScheduledTaskAuditEventV1[]>
}

export function parseScheduledTaskId(value: string): ScheduledTaskId {
  if (!/^[0-9a-f-]{36}$/i.test(value)) throw new TypeError('scheduled task id is invalid')
  return value as ScheduledTaskId
}

export function parseScheduledTaskRunId(value: string): ScheduledTaskRunId {
  if (!/^[0-9a-f-]{36}$/i.test(value)) throw new TypeError('scheduled task run id is invalid')
  return value as ScheduledTaskRunId
}
