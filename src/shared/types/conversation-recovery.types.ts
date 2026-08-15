import type { ConversationId } from './conversation.types'

export const RECOVERY_ACTIONS = [
  'inspect',
  'associateConversation',
  'startEmptyWorkspace',
  'dismissPreservedSource'
] as const

export type RecoveryActionName = (typeof RECOVERY_ACTIONS)[number]
export type RecoveryAuthorizationClass = 'read' | 'mutation'
export type RecoveryStatus =
  | 'unresolved'
  | 'resolvedAssociated'
  | 'resolvedStartedEmpty'
  | 'dismissedPreserved'

export interface RecoveryProvenanceV1 {
  readonly sourceKind: string
  readonly relativePath: string
  readonly sha256: string
  readonly preservedReadOnly: true
}

export interface RecoveryItemV1 {
  readonly recoveryId: string
  readonly kind:
    | 'ambiguous_workspace_manifest'
    | 'identifier_collision'
    | 'invalid_created_at'
    | 'corrupt_source'
    | 'conflicting_worktree_provenance'
    | 'conflicting_session_metadata'
  readonly severity: 'warning' | 'blocking'
  readonly sourcePaths: readonly string[]
  readonly conversationIds: readonly ConversationId[]
  readonly sourceSha256: readonly string[]
  readonly candidateFacts: readonly Readonly<Record<string, unknown>>[]
  readonly provenance: readonly RecoveryProvenanceV1[]
  readonly status: RecoveryStatus
  readonly suggestedActions: readonly RecoveryActionName[]
  readonly revision: number
  readonly associationDecisions: readonly ConversationId[]
}

interface RecoveryRequestCommon {
  recoveryId: string
  /** The RecoveryItem revision, never Conversation.lastSeq or workspace revision. */
  expectedRevision: number
}

export interface InspectRecoveryRequest extends RecoveryRequestCommon {
  action: 'inspect'
  payload: Record<string, never>
  idempotencyKey?: string
}

export interface AssociateConversationRecoveryRequest extends RecoveryRequestCommon {
  action: 'associateConversation'
  idempotencyKey: string
  payload: { conversationId: ConversationId }
}

export interface StartEmptyWorkspaceRecoveryRequest extends RecoveryRequestCommon {
  action: 'startEmptyWorkspace'
  idempotencyKey: string
  payload: {
    conversationId: ConversationId
    expectedWorkspaceRevision: number | null
  }
}

export interface DismissPreservedSourceRecoveryRequest extends RecoveryRequestCommon {
  action: 'dismissPreservedSource'
  idempotencyKey: string
  payload: { reasonCode: 'notApplicable' | 'deferLegacyProjection' }
}

export type RecoveryAction =
  | InspectRecoveryRequest
  | AssociateConversationRecoveryRequest
  | StartEmptyWorkspaceRecoveryRequest
  | DismissPreservedSourceRecoveryRequest

export type ResolveRecoveryItemRequest = RecoveryAction

const uuidPattern = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/

/** Strict boundary decoder: rejects aliases, snake_case fields, mismatched payloads, and extras. */
export function parseResolveRecoveryItemRequest(value: unknown): ResolveRecoveryItemRequest {
  if (!isRecord(value)) throw new TypeError('recovery request must be an object')
  const common = new Set(['recoveryId', 'expectedRevision', 'idempotencyKey', 'action', 'payload'])
  rejectExtraKeys(value, common)
  if (typeof value.recoveryId !== 'string' || value.recoveryId.length === 0) {
    throw new TypeError('recoveryId is required')
  }
  if (!Number.isSafeInteger(value.expectedRevision) || Number(value.expectedRevision) < 1) {
    throw new TypeError('expectedRevision must be a positive RecoveryItem revision')
  }
  if (!isRecord(value.payload)) throw new TypeError('payload must be an object')
  const idempotencyKey = value.idempotencyKey
  if (idempotencyKey !== undefined && !isUuid(idempotencyKey)) {
    throw new TypeError('idempotencyKey must be a canonical UUID')
  }

  switch (value.action) {
    case 'inspect':
      rejectExtraKeys(value.payload, new Set())
      return value as unknown as InspectRecoveryRequest
    case 'associateConversation':
      requireMutationKey(idempotencyKey)
      rejectExtraKeys(value.payload, new Set(['conversationId']))
      requireConversationId(value.payload.conversationId)
      return value as unknown as AssociateConversationRecoveryRequest
    case 'startEmptyWorkspace':
      requireMutationKey(idempotencyKey)
      rejectExtraKeys(value.payload, new Set(['conversationId', 'expectedWorkspaceRevision']))
      requireConversationId(value.payload.conversationId)
      if (
        value.payload.expectedWorkspaceRevision !== null &&
        (!Number.isSafeInteger(value.payload.expectedWorkspaceRevision) ||
          Number(value.payload.expectedWorkspaceRevision) < 1)
      ) {
        throw new TypeError('expectedWorkspaceRevision must be null or a positive integer')
      }
      return value as unknown as StartEmptyWorkspaceRecoveryRequest
    case 'dismissPreservedSource':
      requireMutationKey(idempotencyKey)
      rejectExtraKeys(value.payload, new Set(['reasonCode']))
      if (!['notApplicable', 'deferLegacyProjection'].includes(String(value.payload.reasonCode))) {
        throw new TypeError('invalid dismiss reasonCode')
      }
      return value as unknown as DismissPreservedSourceRecoveryRequest
    default:
      throw new TypeError('unknown recovery action')
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function rejectExtraKeys(value: Record<string, unknown>, allowed: ReadonlySet<string>): void {
  if (Object.keys(value).some((key) => !allowed.has(key))) {
    throw new TypeError('unknown recovery request field')
  }
}

function isUuid(value: unknown): value is string {
  return typeof value === 'string' && uuidPattern.test(value)
}

function requireMutationKey(value: unknown): asserts value is string {
  if (!isUuid(value)) throw new TypeError('mutation action requires a UUID idempotencyKey')
}

function requireConversationId(value: unknown): asserts value is ConversationId {
  if (!isUuid(value)) throw new TypeError('conversationId must be a canonical UUID')
}

export interface RecoveryActionResult {
  recoveryId: string
  action: RecoveryActionName
  authorization: RecoveryAuthorizationClass
  status: RecoveryStatus
  recoveryRevision: number
  workspaceRevision: number | null
  workspaceChanged: boolean
  readonly sourcePaths: readonly string[]
  readonly sourceSha256: readonly string[]
  readonly candidateFacts: readonly Readonly<Record<string, unknown>>[]
  readonly provenance: readonly RecoveryProvenanceV1[]
}

/** One canonical JSON fixture set consumed by both TypeScript and Rust tests. */
export const RECOVERY_ACTION_FIXTURES_JSON = `[
  {
    "request": {
      "recoveryId": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "expectedRevision": 1,
      "action": "inspect",
      "payload": {}
    },
    "authorization": "read",
    "result": {
      "recoveryId": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "action": "inspect",
      "authorization": "read",
      "status": "unresolved",
      "recoveryRevision": 1,
      "workspaceRevision": null,
      "workspaceChanged": false,
      "sourcePaths": ["legacy_workspace_manifests/0/project.json"],
      "sourceSha256": ["eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"],
      "candidateFacts": [{ "candidate": "preserved" }],
      "provenance": [{
        "sourceKind": "legacy_workspace_manifests",
        "relativePath": "legacy_workspace_manifests/0/project.json",
        "sha256": "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        "preservedReadOnly": true
      }]
    }
  },
  {
    "request": {
      "recoveryId": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      "expectedRevision": 2,
      "idempotencyKey": "21aee10a-56b8-4624-a5e7-586c25dc8d1f",
      "action": "associateConversation",
      "payload": { "conversationId": "018f7a1c-1b4d-7c8a-9f01-0123456789ab" }
    },
    "authorization": "mutation",
    "result": {
      "recoveryId": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      "action": "associateConversation",
      "authorization": "mutation",
      "status": "resolvedAssociated",
      "recoveryRevision": 3,
      "workspaceRevision": null,
      "workspaceChanged": false,
      "sourcePaths": ["legacy_workspace_manifests/0/project.json"],
      "sourceSha256": ["eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"],
      "candidateFacts": [{ "candidate": "preserved" }],
      "provenance": [{
        "sourceKind": "legacy_workspace_manifests",
        "relativePath": "legacy_workspace_manifests/0/project.json",
        "sha256": "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        "preservedReadOnly": true
      }]
    }
  },
  {
    "request": {
      "recoveryId": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
      "expectedRevision": 3,
      "idempotencyKey": "d70c2b93-71bc-4df0-85a5-15bd1b7cf452",
      "action": "startEmptyWorkspace",
      "payload": {
        "conversationId": "018f7a1c-1b4d-7c8a-9f01-0123456789ab",
        "expectedWorkspaceRevision": null
      }
    },
    "authorization": "mutation",
    "result": {
      "recoveryId": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
      "action": "startEmptyWorkspace",
      "authorization": "mutation",
      "status": "resolvedStartedEmpty",
      "recoveryRevision": 4,
      "workspaceRevision": 1,
      "workspaceChanged": true,
      "sourcePaths": ["legacy_workspace_manifests/0/project.json"],
      "sourceSha256": ["eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"],
      "candidateFacts": [{ "candidate": "preserved" }],
      "provenance": [{
        "sourceKind": "legacy_workspace_manifests",
        "relativePath": "legacy_workspace_manifests/0/project.json",
        "sha256": "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        "preservedReadOnly": true
      }]
    }
  },
  {
    "request": {
      "recoveryId": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
      "expectedRevision": 4,
      "idempotencyKey": "b025313d-df5d-4254-af4f-535b47ea570f",
      "action": "dismissPreservedSource",
      "payload": { "reasonCode": "deferLegacyProjection" }
    },
    "authorization": "mutation",
    "result": {
      "recoveryId": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
      "action": "dismissPreservedSource",
      "authorization": "mutation",
      "status": "dismissedPreserved",
      "recoveryRevision": 5,
      "workspaceRevision": null,
      "workspaceChanged": false,
      "sourcePaths": ["legacy_workspace_manifests/0/project.json"],
      "sourceSha256": ["eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"],
      "candidateFacts": [{ "candidate": "preserved" }],
      "provenance": [{
        "sourceKind": "legacy_workspace_manifests",
        "relativePath": "legacy_workspace_manifests/0/project.json",
        "sha256": "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        "preservedReadOnly": true
      }]
    }
  }
]`

export const RECOVERY_ACTION_FIXTURES = JSON.parse(RECOVERY_ACTION_FIXTURES_JSON) as readonly {
  request: RecoveryAction
  authorization: RecoveryAuthorizationClass
  result: RecoveryActionResult
}[]
