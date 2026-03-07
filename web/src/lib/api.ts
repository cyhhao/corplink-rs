// API types matching the Rust backend

export type VpnStatus = 'disconnected' | 'connecting' | 'connected' | 'disconnecting' | 'error'

export interface ConnectionInfo {
  status: VpnStatus
  profile: string | null
  vpn_ip: string | null
  peer_address: string | null
  connected_since: string | null
  error: string | null
  server_name: string | null
  use_full_route: boolean | null
  orphan_processes: number
}

export interface ProfileEntry {
  name: string
  company: string
  username: string
  server: string | null
  platform: string | null
  has_password: boolean
  has_totp: boolean
}

export interface ProfileDetail {
  name: string
  company_name: string
  username: string
  platform: string | null
  server: string | null
  has_password: boolean
  has_totp: boolean
  vpn_server_name: string | null
  vpn_select_strategy: string | null
  use_vpn_dns: boolean | null
  use_full_route: boolean | null
  include_private_routes: boolean | null
  extra_routes: string[] | null
}

export interface ProfileFormData {
  company_name: string
  username: string
  password?: string
  platform?: string
  code?: string
  server?: string
  vpn_server_name?: string
  vpn_select_strategy?: string
  use_vpn_dns?: boolean
  use_full_route?: boolean
  include_private_routes?: boolean
  extra_routes?: string[]
}

export interface VersionInfo {
  name: string
  version: string
}

export interface VpnServerEntry {
  name: string
  en_name: string
  ip: string
  vpn_port: number
  protocol: string
}

interface ApiResponse<T> {
  ok: boolean
  data?: T
  error?: string
}

const BASE = ''

async function api<T>(path: string, opts?: RequestInit): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    headers: { 'Content-Type': 'application/json' },
    ...opts,
  })

  // Guard against non-JSON responses (e.g. SPA fallback returning index.html)
  const contentType = res.headers.get('content-type') ?? ''
  if (!contentType.includes('application/json')) {
    throw new Error(
      res.ok
        ? `unexpected response type: ${contentType || 'unknown'}`
        : `server error ${res.status}: ${res.statusText}`,
    )
  }

  const json: ApiResponse<T> = await res.json()
  if (!json.ok) throw new Error(json.error ?? 'unknown error')
  return json.data as T
}

export async function getStatus(): Promise<ConnectionInfo> {
  return api('/api/status')
}

export async function getProfiles(): Promise<ProfileEntry[]> {
  return api('/api/profiles')
}

export async function getProfile(name: string): Promise<ProfileDetail> {
  return api(`/api/profiles/${encodeURIComponent(name)}`)
}

export async function createProfile(name: string, data: ProfileFormData): Promise<ProfileEntry> {
  return api(`/api/profiles/${encodeURIComponent(name)}`, {
    method: 'POST',
    body: JSON.stringify(data),
  })
}

export async function updateProfile(name: string, data: ProfileFormData): Promise<ProfileEntry> {
  return api(`/api/profiles/${encodeURIComponent(name)}`, {
    method: 'PUT',
    body: JSON.stringify(data),
  })
}

export async function deleteProfile(name: string): Promise<void> {
  return api(`/api/profiles/${encodeURIComponent(name)}`, {
    method: 'DELETE',
  })
}

export async function connect(profile: string): Promise<ConnectionInfo> {
  return api('/api/connect', {
    method: 'POST',
    body: JSON.stringify({ profile }),
  })
}

export async function disconnect(): Promise<ConnectionInfo> {
  return api('/api/disconnect', { method: 'POST' })
}

export async function getVersion(): Promise<VersionInfo> {
  return api('/api/version')
}

export async function getVpnServers(profile: string): Promise<VpnServerEntry[]> {
  return api(`/api/vpn-servers/${encodeURIComponent(profile)}`)
}

export async function reconnect(opts: { vpn_server_name?: string; use_full_route?: boolean }): Promise<ConnectionInfo> {
  return api('/api/reconnect', {
    method: 'POST',
    body: JSON.stringify(opts),
  })
}

export async function getLogs(): Promise<string[]> {
  return api('/api/logs')
}

export interface CleanupResult {
  processes_found: number
  processes_cleaned: number
  method: 'none' | 'sentinel' | 'sigterm' | 'sigkill' | 'partial'
  error?: string
}

export async function forceCleanup(): Promise<CleanupResult> {
  return api('/api/force-cleanup', { method: 'POST' })
}
