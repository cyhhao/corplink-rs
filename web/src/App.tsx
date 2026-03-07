import { useEffect, useState, useCallback, useRef } from 'react'
import type { ConnectionInfo, ProfileEntry, ProfileFormData, VersionInfo, VpnServerEntry, VpnStatus, CleanupResult } from './lib/api'
import * as api from './lib/api'
import './index.css'

// ─── Shared small components ────────────────────────────────────────────────

function ToggleSwitch({ checked, onChange, label }: { checked: boolean; onChange: (checked: boolean) => void; label: string }) {
  return (
    <label className="flex items-center gap-2.5 text-sm cursor-pointer select-none">
      <button
        type="button"
        role="switch"
        aria-checked={checked}
        onClick={() => onChange(!checked)}
        className={`relative inline-flex h-[22px] w-[40px] shrink-0 items-center rounded-full transition-colors duration-200 focus:outline-none ${checked ? 'bg-[var(--color-primary)]' : 'bg-gray-600'}`}
      >
        <span
          className={`inline-block h-[16px] w-[16px] rounded-full bg-white shadow transition-transform duration-200 ${checked ? 'translate-x-[20px]' : 'translate-x-[3px]'}`}
        />
      </button>
      {label}
    </label>
  )
}

function StatusBadge({ status }: { status: VpnStatus }) {
  const styles: Record<VpnStatus, string> = {
    connected: 'bg-green-500/20 text-green-400 border-green-500/30',
    connecting: 'bg-blue-500/20 text-blue-400 border-blue-500/30',
    disconnected: 'bg-gray-500/20 text-gray-400 border-gray-500/30',
    disconnecting: 'bg-yellow-500/20 text-yellow-400 border-yellow-500/30',
    error: 'bg-red-500/20 text-red-400 border-red-500/30',
  }
  return (
    <span className={`inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-xs font-medium border ${styles[status]}`}>
      <span className={`w-2 h-2 rounded-full ${status === 'connected' ? 'bg-green-400 animate-pulse' : status === 'connecting' ? 'bg-blue-400 animate-pulse' : status === 'error' ? 'bg-red-400' : 'bg-gray-400'}`} />
      {status.charAt(0).toUpperCase() + status.slice(1)}
    </span>
  )
}

function InfoRow({ label, value }: { label: string; value: string | null | undefined }) {
  return (
    <div className="flex justify-between">
      <span className="text-[var(--color-text-secondary)]">{label}</span>
      <span className="font-mono text-xs">{value ?? '—'}</span>
    </div>
  )
}

function formatElapsed(since: Date): string {
  const seconds = Math.floor((Date.now() - since.getTime()) / 1000)
  if (seconds < 60) return `${seconds}s`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes}m ${seconds % 60}s`
  const hours = Math.floor(minutes / 60)
  return `${hours}h ${minutes % 60}m`
}

const inputCls = 'w-full px-3 py-2 rounded-lg bg-[var(--color-background)] border border-[var(--color-border)] text-sm text-[var(--color-text)] placeholder:text-[var(--color-text-secondary)] focus:outline-none focus:border-[var(--color-primary)]'
const labelCls = 'block text-xs font-medium text-[var(--color-text-secondary)] mb-1'
const btnPrimary = 'px-4 py-2 rounded-lg bg-[var(--color-primary)] text-white text-sm font-medium hover:bg-[var(--color-primary-hover)] disabled:opacity-40 disabled:cursor-not-allowed cursor-pointer transition-colors'
const btnSecondary = 'px-4 py-2 rounded-lg bg-[var(--color-surface)] text-[var(--color-text)] text-sm font-medium border border-[var(--color-border)] hover:bg-[var(--color-surface-hover)] cursor-pointer transition-colors'
const btnDanger = 'px-4 py-2 rounded-lg bg-red-500/10 text-red-400 text-sm font-medium border border-red-500/20 hover:bg-red-500/20 cursor-pointer transition-colors'

// ─── Connection Card ────────────────────────────────────────────────────────

function ConnectionCard({ info, onDisconnect, onReconnect, onForceCleanup }: {
  info: ConnectionInfo
  onDisconnect: () => void
  onReconnect: (opts: { vpn_server_name?: string; use_full_route?: boolean }) => void
  onForceCleanup: () => Promise<void>
}) {
  const elapsed = info.connected_since ? formatElapsed(new Date(info.connected_since)) : null
  const [showSettings, setShowSettings] = useState(false)
  const [vpnServers, setVpnServers] = useState<VpnServerEntry[]>([])
  const [vpnLoading, setVpnLoading] = useState(false)
  const [selectedServer, setSelectedServer] = useState<string>(info.server_name ?? '')
  const [fullRoute, setFullRoute] = useState(info.use_full_route ?? false)
  const [showLogs, setShowLogs] = useState(false)
  const [logs, setLogs] = useState<string[]>([])
  const logsEndRef = useRef<HTMLDivElement>(null)
  const [cleaning, setCleaning] = useState(false)
  const [cleanupResult, setCleanupResult] = useState<CleanupResult | null>(null)
  const [cleanupError, setCleanupError] = useState<string | null>(null)

  // Grace period: only show orphan warning after status has been
  // disconnected/error for 5+ seconds (avoids flash during reconnect).
  const [orphanGracePassed, setOrphanGracePassed] = useState(false)
  const graceTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)

  useEffect(() => {
    const isIdleWithOrphans =
      (info.status === 'disconnected' || info.status === 'error') && info.orphan_processes > 0

    if (isIdleWithOrphans) {
      // Start a 5-second timer; if still idle when it fires, show the banner.
      if (!graceTimerRef.current) {
        graceTimerRef.current = setTimeout(() => {
          setOrphanGracePassed(true)
          graceTimerRef.current = null
        }, 5000)
      }
    } else {
      // Status recovered (connecting/connected) — cancel timer, hide banner.
      if (graceTimerRef.current) {
        clearTimeout(graceTimerRef.current)
        graceTimerRef.current = null
      }
      setOrphanGracePassed(false)
    }

    return () => {
      if (graceTimerRef.current) {
        clearTimeout(graceTimerRef.current)
        graceTimerRef.current = null
      }
    }
  }, [info.status, info.orphan_processes])

  // Reset stale cleanup result when orphan count changes (not on status
  // transitions — the Disconnecting→Disconnected flip during cleanup would
  // otherwise wipe the result before the user sees it).
  useEffect(() => {
    setCleanupResult(null)
    setCleanupError(null)
  }, [info.orphan_processes])

  // Sync state when info changes (e.g. after reconnect completes)
  useEffect(() => {
    setSelectedServer(info.server_name ?? '')
    setFullRoute(info.use_full_route ?? false)
  }, [info.server_name, info.use_full_route])

  // Auto-scroll logs
  useEffect(() => {
    if (showLogs) logsEndRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [logs, showLogs])

  // Poll logs when panel is open
  useEffect(() => {
    if (!showLogs) return
    const load = () => api.getLogs().then(setLogs).catch(() => {})
    load()
    const iv = setInterval(load, 2000)
    return () => clearInterval(iv)
  }, [showLogs])

  const fetchServers = async () => {
    if (!info.profile) return
    setVpnLoading(true)
    try {
      const servers = await api.getVpnServers(info.profile)
      setVpnServers(servers)
    } catch { /* ignore */ } finally {
      setVpnLoading(false)
    }
  }

  // Auto-fetch VPN servers when settings panel is opened
  useEffect(() => {
    if (showSettings && vpnServers.length === 0) {
      fetchServers()
    }
  }, [showSettings])

  // Pre-fetch VPN servers once when connection is first established,
  // so the list is ready when the user opens Settings.
  const hasFetchedRef = useRef(false)
  useEffect(() => {
    if (info.status === 'connected' && !hasFetchedRef.current) {
      hasFetchedRef.current = true
      fetchServers()
    }
    if (info.status !== 'connected') {
      hasFetchedRef.current = false
    }
  }, [info.status])

  const hasChanges = selectedServer !== (info.server_name ?? '') || fullRoute !== (info.use_full_route ?? false)

  const applyChanges = () => {
    onReconnect({
      vpn_server_name: selectedServer || undefined,
      use_full_route: fullRoute,
    })
    setShowSettings(false)
  }

  return (
    <div className="rounded-xl border border-[var(--color-border)] bg-[var(--color-surface)] p-6">
      <div className="flex items-center justify-between mb-4">
        <h2 className="text-lg font-semibold">Connection</h2>
        <div className="flex items-center gap-2">
          <button onClick={() => setShowLogs(!showLogs)} className="text-xs px-2 py-1 rounded-lg border border-[var(--color-border)] hover:bg-[var(--color-background)] cursor-pointer" title="Toggle logs">
            Logs
          </button>
          <StatusBadge status={info.status} />
        </div>
      </div>

      {info.status === 'connected' && (
        <div className="space-y-3 text-sm">
          <InfoRow label="Profile" value={info.profile} />
          <InfoRow label="Server" value={info.server_name} />
          <InfoRow label="VPN IP" value={info.vpn_ip} />
          <InfoRow label="Peer" value={info.peer_address} />
          <InfoRow label="Route" value={info.use_full_route ? 'Full (all traffic)' : 'Split'} />
          <InfoRow label="Uptime" value={elapsed} />

          <div className="flex gap-2 mt-4">
            <button onClick={() => setShowSettings(!showSettings)} className={`flex-1 py-2 ${btnSecondary}`}>
              {showSettings ? 'Hide Settings' : 'Settings'}
            </button>
            <button onClick={onDisconnect} className={`flex-1 py-2 ${btnDanger}`}>
              Disconnect
            </button>
          </div>

          {showSettings && (
            <div className="mt-3 p-4 rounded-lg border border-[var(--color-border)] bg-[var(--color-background)] space-y-3">
              <div>
                <label className="block text-xs font-medium mb-1 text-[var(--color-text-secondary)]">VPN Server</label>
                <div className="flex gap-2">
                  {vpnServers.length > 0 ? (
                    <select className={inputCls + ' flex-1 !text-sm'} value={selectedServer} onChange={(e) => setSelectedServer(e.target.value)}>
                      <option value="">Auto-select (first available)</option>
                      {vpnServers.map((s) => (
                        <option key={`${s.name}-${s.ip}`} value={s.name}>
                          {s.name || s.en_name} ({s.ip}:{s.vpn_port} {s.protocol})
                        </option>
                      ))}
                    </select>
                  ) : (
                    <input className={inputCls + ' flex-1 !text-sm'} value={selectedServer} onChange={(e) => setSelectedServer(e.target.value)} placeholder="Server name (fetch to see list)" />
                  )}
                  <button type="button" onClick={fetchServers} disabled={vpnLoading} className="px-3 py-1.5 rounded-lg text-xs font-medium bg-[var(--color-primary)] text-white hover:opacity-90 disabled:opacity-50 whitespace-nowrap cursor-pointer disabled:cursor-not-allowed">
                    {vpnLoading ? '...' : 'Fetch'}
                  </button>
                </div>
              </div>
              <ToggleSwitch checked={fullRoute} onChange={setFullRoute} label="Full route mode (all traffic through VPN)" />
              {hasChanges && (
                <button onClick={applyChanges} className={`w-full py-2 ${btnPrimary}`}>
                  Apply &amp; Reconnect
                </button>
              )}
            </div>
          )}
        </div>
      )}

      {info.status === 'connecting' && (
        <p className="text-sm text-[var(--color-text-secondary)]">
          Connecting to <strong>{info.profile}</strong>...
        </p>
      )}

      {info.status === 'disconnected' && (
        <p className="text-sm text-[var(--color-text-secondary)]">
          No active connection. Select a profile below to connect.
        </p>
      )}

      {info.status === 'error' && info.error && (
        <p className="text-sm text-red-400 bg-red-500/10 rounded-lg p-3 border border-red-500/20">
          {info.error}
        </p>
      )}

      {/* Orphan process warning & cleanup */}
      {(orphanGracePassed || cleaning) && (
        <div className="mt-3 p-4 rounded-lg border border-yellow-500/30 bg-yellow-500/10 space-y-2">
          <p className="text-sm text-yellow-400">
            Detected <strong>{info.orphan_processes}</strong> orphan VPN daemon process{info.orphan_processes > 1 ? 'es' : ''} running in the background.
            This may leave VPN routes and DNS settings in an inconsistent state.
          </p>
          {cleanupResult && (
            <p className="text-xs text-[var(--color-text-secondary)]">
              Cleanup: {cleanupResult.processes_cleaned}/{cleanupResult.processes_found} killed
              {cleanupResult.method !== 'none' && ` (via ${cleanupResult.method})`}
            </p>
          )}
          {(cleanupError || cleanupResult?.error) && (
            <p className="text-xs text-red-400">
              {cleanupError || cleanupResult?.error}
            </p>
          )}
          <button
            onClick={async () => {
              setCleaning(true)
              setCleanupResult(null)
              setCleanupError(null)
              try {
                const result = await api.forceCleanup()
                setCleanupResult(result)
              } catch (e) {
                setCleanupError(e instanceof Error ? e.message : 'cleanup failed')
              }
              await onForceCleanup()
              setCleaning(false)
            }}
            disabled={cleaning}
            className={`w-full py-2 ${btnDanger} ${cleaning ? '!opacity-60' : ''}`}
          >
            {cleaning ? 'Cleaning up...' : 'Clean Up VPN Processes'}
          </button>
          {!cleaning && !cleanupResult && (
            <p className="text-[11px] text-[var(--color-text-secondary)]">
              Tries graceful shutdown first (preserves DNS/routes), then escalates to force kill if needed.
            </p>
          )}
        </div>
      )}

      {/* Log panel */}
      {showLogs && (
        <div className="mt-4 rounded-lg border border-[var(--color-border)] bg-[var(--color-background)]">
          <div className="flex items-center justify-between px-3 py-2 border-b border-[var(--color-border)]">
            <span className="text-xs font-medium text-[var(--color-text-secondary)]">Logs ({logs.length} lines)</span>
            <button onClick={() => setShowLogs(false)} className="text-xs text-[var(--color-text-secondary)] hover:text-[var(--color-text)] cursor-pointer">&times;</button>
          </div>
          <div className="max-h-64 overflow-y-auto p-3 font-mono text-[11px] leading-relaxed text-[var(--color-text-secondary)] whitespace-pre-wrap break-all">
            {logs.length === 0 ? <span className="opacity-50">No logs yet</span> : logs.map((line, i) => <div key={i}>{line}</div>)}
            <div ref={logsEndRef} />
          </div>
        </div>
      )}
    </div>
  )
}

// ─── Profile Card (in list) ─────────────────────────────────────────────────

function ProfileCard({
  profile, isActive, onConnect, onEdit, onDelete, disabled,
}: {
  profile: ProfileEntry
  isActive: boolean
  onConnect: () => void
  onEdit: () => void
  onDelete: () => void
  disabled: boolean
}) {
  const [confirmDelete, setConfirmDelete] = useState(false)

  return (
    <div className={`rounded-xl border p-4 transition-colors ${isActive ? 'border-green-500/50 bg-green-500/5' : 'border-[var(--color-border)] bg-[var(--color-surface)]'}`}>
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <h3 className="font-medium truncate">{profile.name}</h3>
            {profile.platform && (
              <span className="text-[10px] px-1.5 py-0.5 rounded bg-[var(--color-background)] text-[var(--color-text-secondary)] border border-[var(--color-border)]">
                {profile.platform}
              </span>
            )}
          </div>
          <p className="text-xs text-[var(--color-text-secondary)] mt-0.5">
            {profile.username} @ {profile.company}
          </p>
          {profile.server && (
            <p className="text-xs text-[var(--color-text-secondary)] font-mono mt-0.5 truncate">
              {profile.server}
            </p>
          )}
          <div className="flex gap-2 mt-1.5">
            {profile.has_password && <span className="text-[10px] text-green-400">password</span>}
            {profile.has_totp && <span className="text-[10px] text-blue-400">totp</span>}
          </div>
        </div>

        <div className="flex items-center gap-2 shrink-0">
          <button onClick={onEdit} className="text-xs text-[var(--color-text-secondary)] hover:text-[var(--color-text)] transition-colors px-2 py-1 cursor-pointer">
            Edit
          </button>
          {!isActive && (
            confirmDelete ? (
              <div className="flex gap-1">
                <button onClick={onDelete} className="text-xs text-red-400 hover:text-red-300 px-1 cursor-pointer">Yes</button>
                <button onClick={() => setConfirmDelete(false)} className="text-xs text-[var(--color-text-secondary)] px-1 cursor-pointer">No</button>
              </div>
            ) : (
              <button onClick={() => setConfirmDelete(true)} className="text-xs text-[var(--color-text-secondary)] hover:text-red-400 transition-colors px-2 py-1 cursor-pointer">
                Delete
              </button>
            )
          )}
          {isActive ? (
            <span className="text-xs text-green-400 font-medium px-2">Active</span>
          ) : (
            <button onClick={onConnect} disabled={disabled} className={btnPrimary}>
              Connect
            </button>
          )}
        </div>
      </div>
    </div>
  )
}

// ─── Profile Form (create / edit) ───────────────────────────────────────────

const PLATFORMS = [
  { value: '', label: 'Auto-detect' },
  { value: 'feilian', label: 'CorpLink (feilian)' },
  { value: 'ldap', label: 'LDAP' },
  { value: 'lark', label: 'Lark (Feishu)' },
  { value: 'OIDC', label: 'OIDC' },
]

function ProfileForm({
  editName,
  onSave,
  onCancel,
}: {
  editName: string | null // null = create mode
  onSave: () => void
  onCancel: () => void
}) {
  const isEdit = editName !== null
  const [name, setName] = useState(editName ?? '')
  const [form, setForm] = useState<ProfileFormData>({
    company_name: '',
    username: '',
    use_vpn_dns: true,
    extra_routes: ['10.0.0.0/8'],
  })
  const [showAdvanced, setShowAdvanced] = useState(false)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(isEdit)
  const [vpnServers, setVpnServers] = useState<VpnServerEntry[]>([])
  const [vpnServersLoading, setVpnServersLoading] = useState(false)
  const [vpnServersError, setVpnServersError] = useState<string | null>(null)

  // Load existing data when editing
  useEffect(() => {
    if (!isEdit || !editName) return
    setLoading(true)
    api.getProfile(editName).then((detail) => {
      setForm({
        company_name: detail.company_name,
        username: detail.username,
        platform: detail.platform ?? undefined,
        server: detail.server ?? undefined,
        vpn_server_name: detail.vpn_server_name ?? undefined,
        vpn_select_strategy: detail.vpn_select_strategy ?? undefined,
        use_vpn_dns: detail.use_vpn_dns ?? undefined,
        use_full_route: detail.use_full_route ?? undefined,
        include_private_routes: detail.include_private_routes ?? undefined,
        extra_routes: detail.extra_routes ?? undefined,
      })
      setLoading(false)
    }).catch((e) => {
      setError(e instanceof Error ? e.message : 'load failed')
      setLoading(false)
    })
  }, [isEdit, editName])

  // Fetch available VPN servers for this profile (requires login, may take a few seconds)
  const fetchVpnServers = async () => {
    const profileName = isEdit ? editName! : name.trim()
    if (!profileName) { setVpnServersError('Save profile first'); return }
    setVpnServersLoading(true)
    setVpnServersError(null)
    try {
      const servers = await api.getVpnServers(profileName)
      setVpnServers(servers)
    } catch (e) {
      setVpnServersError(e instanceof Error ? e.message : 'failed to fetch servers')
    } finally {
      setVpnServersLoading(false)
    }
  }

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()
    if (!name.trim()) { setError('Profile name is required'); return }
    if (!form.company_name.trim()) { setError('Company name is required'); return }
    if (!form.username.trim()) { setError('Username is required'); return }

    setSaving(true)
    setError(null)
    try {
      if (isEdit) {
        await api.updateProfile(name, form)
      } else {
        await api.createProfile(name.trim(), form)
      }
      onSave()
    } catch (e) {
      setError(e instanceof Error ? e.message : 'save failed')
    } finally {
      setSaving(false)
    }
  }

  const update = (patch: Partial<ProfileFormData>) => setForm((f) => ({ ...f, ...patch }))

  if (loading) {
    return (
      <div className="rounded-xl border border-[var(--color-border)] bg-[var(--color-surface)] p-6">
        <p className="text-sm text-[var(--color-text-secondary)]">Loading...</p>
      </div>
    )
  }

  return (
    <form onSubmit={handleSubmit} className="rounded-xl border border-[var(--color-border)] bg-[var(--color-surface)] p-6 space-y-4">
      <h2 className="text-lg font-semibold mb-2">{isEdit ? `Edit "${editName}"` : 'New Profile'}</h2>

      {error && (
        <div className="p-2.5 rounded-lg bg-red-500/10 border border-red-500/20 text-red-400 text-sm">
          {error}
        </div>
      )}

      {/* Profile name (only for create) */}
      {!isEdit && (
        <div>
          <label className={labelCls}>Profile Name *</label>
          <input className={inputCls} value={name} onChange={(e) => setName(e.target.value)} placeholder="e.g. office-bj" required />
        </div>
      )}

      {/* Essential fields */}
      <div className="grid grid-cols-2 gap-3">
        <div>
          <label className={labelCls}>Company Name *</label>
          <input className={inputCls} value={form.company_name} onChange={(e) => update({ company_name: e.target.value })} placeholder="Your company code" required />
        </div>
        <div>
          <label className={labelCls}>Username *</label>
          <input className={inputCls} value={form.username} onChange={(e) => update({ username: e.target.value })} placeholder="Login username or email" required />
        </div>
      </div>

      <div className="grid grid-cols-2 gap-3">
        <div>
          <label className={labelCls}>Password (MD5 hash) {isEdit ? '(leave empty to keep)' : ''}</label>
          <input className={inputCls} type="password" value={form.password ?? ''} onChange={(e) => update({ password: e.target.value || undefined })} placeholder="MD5 hashed password (32 hex chars)" />
        </div>
        <div>
          <label className={labelCls}>Platform</label>
          <select className={inputCls} value={form.platform ?? ''} onChange={(e) => update({ platform: e.target.value || undefined })}>
            {PLATFORMS.map((p) => <option key={p.value} value={p.value}>{p.label}</option>)}
          </select>
        </div>
      </div>

      <div>
        <label className={labelCls}>TOTP Secret {isEdit ? '(leave empty to keep)' : '(optional)'}</label>
        <input className={inputCls} value={form.code ?? ''} onChange={(e) => update({ code: e.target.value || undefined })} placeholder="Base32 TOTP secret for auto 2FA" />
      </div>

      {/* Advanced toggle */}
      <button type="button" onClick={() => setShowAdvanced(!showAdvanced)} className="text-xs text-[var(--color-primary)] hover:underline cursor-pointer">
        {showAdvanced ? 'Hide' : 'Show'} advanced options
      </button>

      {showAdvanced && (
        <div className="space-y-3 pt-2 border-t border-[var(--color-border)]">
          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className={labelCls}>Server URL</label>
              <input className={inputCls} value={form.server ?? ''} onChange={(e) => update({ server: e.target.value || undefined })} placeholder="Auto-resolved if empty" />
            </div>
            <div>
              <label className={labelCls}>VPN Server Name</label>
              <div className="flex gap-2">
                {vpnServers.length > 0 ? (
                  <select className={inputCls + ' flex-1'} value={form.vpn_server_name ?? ''} onChange={(e) => update({ vpn_server_name: e.target.value || undefined })}>
                    <option value="">Auto-select (first available)</option>
                    {vpnServers.map((s) => (
                      <option key={`${s.name}-${s.ip}`} value={s.name}>
                        {s.name || s.en_name} ({s.ip}:{s.vpn_port} {s.protocol})
                      </option>
                    ))}
                  </select>
                ) : (
                  <input className={inputCls + ' flex-1'} value={form.vpn_server_name ?? ''} onChange={(e) => update({ vpn_server_name: e.target.value || undefined })} placeholder="Auto-select if empty" />
                )}
                <button
                  type="button"
                  onClick={fetchVpnServers}
                  disabled={vpnServersLoading || (!isEdit && !name.trim())}
                  className="px-3 py-1.5 rounded-lg text-xs font-medium bg-[var(--color-primary)] text-white hover:opacity-90 disabled:opacity-50 whitespace-nowrap cursor-pointer disabled:cursor-not-allowed"
                >
                  {vpnServersLoading ? 'Loading...' : 'Fetch'}
                </button>
              </div>
              {vpnServersError && <p className="text-xs text-red-400 mt-1">{vpnServersError}</p>}
            </div>
          </div>

          <div>
            <label className={labelCls}>Select Strategy</label>
            <select className={inputCls} value={form.vpn_select_strategy ?? ''} onChange={(e) => update({ vpn_select_strategy: e.target.value || undefined })}>
              <option value="">Default (first available)</option>
              <option value="latency">Latency (fastest)</option>
              <option value="default">Default</option>
            </select>
          </div>

          <div className="flex flex-wrap gap-4">
            <label className="flex items-center gap-2 text-sm">
              <input type="checkbox" checked={form.use_vpn_dns ?? false} onChange={(e) => update({ use_vpn_dns: e.target.checked })} className="rounded" />
              Use VPN DNS
            </label>
            <ToggleSwitch checked={form.use_full_route ?? false} onChange={(v) => update({ use_full_route: v })} label="Full route mode" />
            <label className="flex items-center gap-2 text-sm">
              <input type="checkbox" checked={form.include_private_routes ?? true} onChange={(e) => update({ include_private_routes: e.target.checked })} className="rounded" />
              Include private routes
            </label>
          </div>

          <div>
            <label className={labelCls}>Extra Routes (one per line)</label>
            <textarea
              className={`${inputCls} min-h-[60px]`}
              value={(form.extra_routes ?? []).join('\n')}
              onChange={(e) => {
                const lines = e.target.value.split('\n').map((l) => l.trim()).filter(Boolean)
                update({ extra_routes: lines.length > 0 ? lines : undefined })
              }}
              placeholder="e.g. 10.0.0.0/8"
              rows={3}
            />
          </div>
        </div>
      )}

      <div className="flex gap-3 pt-2">
        <button type="submit" disabled={saving} className={btnPrimary}>
          {saving ? 'Saving...' : isEdit ? 'Save Changes' : 'Create Profile'}
        </button>
        <button type="button" onClick={onCancel} className={btnSecondary}>Cancel</button>
      </div>
    </form>
  )
}

// ─── Empty State (no profiles) ──────────────────────────────────────────────

function EmptyState({ onCreate }: { onCreate: () => void }) {
  return (
    <div className="rounded-xl border border-dashed border-[var(--color-border)] bg-[var(--color-surface)] p-8 text-center">
      <div className="text-4xl mb-4 opacity-40">VPN</div>
      <h2 className="text-lg font-semibold mb-2">Welcome to CorpLink</h2>
      <p className="text-sm text-[var(--color-text-secondary)] mb-6 max-w-sm mx-auto">
        No VPN profiles configured yet. Create your first profile to connect to your company's VPN.
      </p>
      <button onClick={onCreate} className={btnPrimary}>
        Create Your First Profile
      </button>
    </div>
  )
}

// ─── App ────────────────────────────────────────────────────────────────────

type View = { type: 'main' } | { type: 'create' } | { type: 'edit'; name: string }

export default function App() {
  const [info, setInfo] = useState<ConnectionInfo | null>(null)
  const [profiles, setProfiles] = useState<ProfileEntry[]>([])
  const [version, setVersion] = useState<VersionInfo | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [view, setView] = useState<View>({ type: 'main' })

  const refresh = useCallback(async () => {
    try {
      const [status, profs] = await Promise.all([api.getStatus(), api.getProfiles()])
      setInfo(status)
      setProfiles(profs)
      setError(null)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'failed to reach daemon')
    }
  }, [])

  useEffect(() => {
    refresh()
    api.getVersion().then(setVersion).catch(() => {})
    const interval = setInterval(refresh, 2000)
    return () => clearInterval(interval)
  }, [refresh])

  const handleConnect = async (profileName: string) => {
    try {
      await api.connect(profileName)
      await refresh()
    } catch (e) {
      setError(e instanceof Error ? e.message : 'connect failed')
    }
  }

  const handleDisconnect = async () => {
    try {
      await api.disconnect()
      await refresh()
    } catch (e) {
      setError(e instanceof Error ? e.message : 'disconnect failed')
    }
  }

  const handleReconnect = async (opts: { vpn_server_name?: string; use_full_route?: boolean }) => {
    try {
      await api.reconnect(opts)
      await refresh()
    } catch (e) {
      setError(e instanceof Error ? e.message : 'reconnect failed')
    }
  }

  const handleDelete = async (name: string) => {
    try {
      await api.deleteProfile(name)
      await refresh()
    } catch (e) {
      setError(e instanceof Error ? e.message : 'delete failed')
    }
  }

  const handleFormSave = () => {
    setView({ type: 'main' })
    refresh()
  }

  const busy = info?.status === 'connecting' || info?.status === 'disconnecting'

  return (
    <div className="min-h-screen p-6 max-w-xl mx-auto">
      {/* Header */}
      <header className="flex items-center justify-between mb-8">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">CorpLink</h1>
          {version && (
            <p className="text-xs text-[var(--color-text-secondary)] mt-0.5">v{version.version}</p>
          )}
        </div>
        {info && <StatusBadge status={info.status} />}
      </header>

      {/* Global error */}
      {error && (
        <div className="mb-6 p-3 rounded-lg bg-red-500/10 border border-red-500/20 text-red-400 text-sm flex justify-between items-center">
          <span>{error}</span>
          <button onClick={() => setError(null)} className="text-red-400/60 hover:text-red-400 ml-3 cursor-pointer">&times;</button>
        </div>
      )}

      {/* Create / Edit form */}
      {view.type === 'create' && (
        <div className="mb-8">
          <ProfileForm editName={null} onSave={handleFormSave} onCancel={() => setView({ type: 'main' })} />
        </div>
      )}

      {view.type === 'edit' && (
        <div className="mb-8">
          <ProfileForm editName={view.name} onSave={handleFormSave} onCancel={() => setView({ type: 'main' })} />
        </div>
      )}

      {/* Main view */}
      {view.type === 'main' && (
        <>
          {/* Connection status card */}
          {info && <ConnectionCard info={info} onDisconnect={handleDisconnect} onReconnect={handleReconnect} onForceCleanup={async () => { await refresh() }} />}

          {/* Profiles */}
          <div className="mt-8">
            <div className="flex items-center justify-between mb-4">
              <h2 className="text-lg font-semibold">Profiles</h2>
              {profiles.length > 0 && (
                <button onClick={() => setView({ type: 'create' })} className={`${btnPrimary} !py-1.5 !px-3 !text-xs`}>
                  + New
                </button>
              )}
            </div>

            {profiles.length === 0 ? (
              <EmptyState onCreate={() => setView({ type: 'create' })} />
            ) : (
              <div className="space-y-3">
                {profiles.map((p) => (
                  <ProfileCard
                    key={p.name}
                    profile={p}
                    isActive={info?.profile === p.name && info?.status === 'connected'}
                    onConnect={() => handleConnect(p.name)}
                    onEdit={() => setView({ type: 'edit', name: p.name })}
                    onDelete={() => handleDelete(p.name)}
                    disabled={busy}
                  />
                ))}
              </div>
            )}
          </div>
        </>
      )}

      {/* Footer */}
      <footer className="mt-12 text-center text-xs text-[var(--color-text-secondary)]">
        corplink — open-source CorpLink VPN client
      </footer>
    </div>
  )
}
