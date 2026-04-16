import { Fragment, useEffect, useState } from 'react'
import {
  LineChart, Line, XAxis, YAxis, CartesianGrid, Tooltip, ResponsiveContainer,
} from 'recharts'
import './App.css'

// ── Shared types ──────────────────────────────────────────────────────────────

interface Stats {
  total_cycles: number
  triggered_cycles: number
  trigger_rate: number
  win_rate: number
  total_pnl_usdc: number
  as_of: string
}

interface DailyWinRate {
  date: string
  win_rate: number
  n: number
}

// ── BTC 15m types ─────────────────────────────────────────────────────────────

interface StrategySummary {
  strategy_id: string
  total_cycles: number
  triggered_cycles: number
  total_trades: number
  trigger_rate: number
  win_rate: number
  total_pnl_usdc: number
  total_invested_usdc: number
  total_fees_usdc: number
  capital_allocation_pct: number
  initial_allocated_usdc: number
  pnl_pct_alloc: number
  pnl_pct_invested: number
}

interface StrategyDetail {
  strategy_id: string
  days: number
  capital_allocation_pct: number
  initial_allocated_usdc: number
  total_cycles: number
  triggered_cycles: number
  total_trades: number
  win_rate: number
  total_pnl_usdc: number
  total_invested_usdc: number
  total_fees_usdc: number
  pnl_pct_alloc: number
  pnl_pct_invested: number
  avg_leg1_price: number | null
  avg_leg2_price: number | null
  recent_cycles: {
    id: number; market_slug: string
    leg1_price: number | null; leg2_price: number | null
    pnl_usdc: number | null; created_at: string
  }[]
  recent_trades: {
    id: number; market_slug: string; leg: number; side: string
    price: number; size_usdc: number; fee_usdc: number
    would_profit: number | null; ts: string
  }[]
}

interface Cycle {
  id: number; market_slug: string; mode: string
  leg1_triggered: boolean
  leg1_price: number | null; leg2_price: number | null
  pnl_usdc: number | null; resolved_winner: string | null; created_at: string
}

interface DryRun {
  id: number; ts: string; market_slug: string; leg: number; side: string
  price: number; size_usdc: number
  signal_dump_pct: number | null; hedge_sum: number | null; would_profit: number | null
}

// ── Mention types ─────────────────────────────────────────────────────────────

interface MentionStats {
  days: number
  total_rows: number
  entry_count: number
  cancel_count: number
  trigger_rate: number
  fill_rate: number
  avg_hold_sec: number | null
  max_hold_sec: number | null
  net_pnl_usdc: number
  gross_pnl_usdc: number
  max_drawdown_pct: number
  sharpe_ratio: number
  max_consecutive_losses: number
  edge_too_low_count: number
  spread_too_wide_count: number
  depth_too_thin_count: number
  as_of: string
}

interface MentionStrategySummary {
  strategy_id: string
  entry_count: number
  cancel_count: number
  trigger_rate: number
  fill_rate: number
  net_pnl_usdc: number
  capital_allocation_pct: number
  initial_allocated_usdc: number
  pnl_pct_alloc: number
  active_keywords: string[]
  active_markets: string[]
  total_markets: number
}

interface MentionStrategyDetail {
  strategy_id: string
  days: number
  capital_allocation_pct: number
  initial_allocated_usdc: number
  total_signals: number
  entry_count: number
  cancel_count: number
  total_trades: number
  trigger_rate: number
  fill_rate: number
  win_rate: number
  net_pnl_usdc: number
  total_invested_usdc: number
  pnl_pct_alloc: number
  pnl_pct_invested: number
  avg_entry_price: number | null
  avg_exit_price: number | null
  avg_hold_sec: number | null
  tp_count: number
  sl_count: number
  te_count: number
  active_keywords: string[]
  active_markets: string[]
  recent_trades: {
    id: number; ts: string; market_slug: string; keyword: string; side: string
    action: string; price: number; size_usdc: number; hold_sec: number | null
    expected_net_edge_bps: number | null; realized_pnl_usdc: number | null; reason_code: string
  }[]
}


// ── Weather Ladder types ──────────────────────────────────────────────────────

interface WeatherLadderStrategySummary {
  strategy_id: string
  total_ladders: number
  open_ladders: number
  cat_exit_count: number
  total_invested_usdc: number
  avg_payout_ratio: number
  net_pnl_usdc: number
  capital_allocation_pct: number
  initial_allocated_usdc: number
  pnl_pct_alloc: number
  active_cities: string[]
  total_cities: number
}

interface OpenLadder {
  ladder_id: string
  city: string
  target_date: string
  legs: number
  sum_price: number
  payout_ratio: number
  combined_p: number
  total_usdc: number
  model: string
  lead_days: number | null
  entry_ts: string
}

interface WeatherLadderStrategyDetail {
  strategy_id: string
  days: number
  capital_allocation_pct: number
  initial_allocated_usdc: number
  total_ladders: number
  open_ladders: number
  htr_count: number
  cat_count: number
  avg_payout_ratio: number
  net_pnl_usdc: number
  total_invested_usdc: number
  pnl_pct_alloc: number
  open_ladders_list: OpenLadder[]
  recent_trades: {
    id: number; ts: string; ladder_id: string; city: string
    target_date: string; action: string; leg_index: number
    price: number; size_usdc: number; p_yes: number | null
    lead_days: number | null; ladder_legs: number
    ladder_sum_price: number; payout_ratio: number; combined_p: number
    realized_pnl_usdc: number | null; model: string; reason_code: string
  }[]
}

// ── Weather types ─────────────────────────────────────────────────────────────

interface WeatherStats {
  days: number
  total_rows: number
  entry_count: number
  cancel_count: number
  trigger_rate: number
  fill_rate: number
  avg_hold_sec: number | null
  max_hold_sec: number | null
  net_pnl_usdc: number
  gross_pnl_usdc: number
  win_rate_overall: number
  max_drawdown_pct: number
  sharpe_ratio: number
  max_consecutive_losses: number
  low_edge_count: number
  low_confidence_count: number
  spread_wide_count: number
  low_depth_count: number
  exits: {
    take_profit: number
    stop_loss: number
    forecast_shift: number
    time_decay_exit: number
  }
  forecast_shift_analysis: {
    count: number
    avg_p_yes_delta: number | null
    pct_direction_flipped: number | null
  }
  as_of: string
}

interface WeatherStrategySummary {
  strategy_id: string
  entry_count: number
  cancel_count: number
  trigger_rate: number
  fill_rate: number
  net_pnl_usdc: number
  capital_allocation_pct: number
  initial_allocated_usdc: number
  pnl_pct_alloc: number
  active_cities: string[]
  total_cities: number
}

interface WeatherStrategyDetail {
  strategy_id: string
  days: number
  capital_allocation_pct: number
  initial_allocated_usdc: number
  total_signals: number
  entry_count: number
  cancel_count: number
  total_trades: number
  trigger_rate: number
  fill_rate: number
  win_rate: number
  net_pnl_usdc: number
  total_invested_usdc: number
  pnl_pct_alloc: number
  pnl_pct_invested: number
  avg_entry_price: number | null
  avg_exit_price: number | null
  avg_hold_sec: number | null
  tp_count: number
  sl_count: number
  fs_count: number
  td_count: number
  active_cities: string[]
  open_positions: {
    id: number; ts: string; market_slug: string; city: string
    side: string; price: number; p_yes_at_entry: number | null
    lead_days: number | null; model: string
  }[]
  recent_trades: {
    id: number; ts: string; market_slug: string; city: string; market_type: string
    side: string; action: string; price: number; size_usdc: number; hold_sec: number | null
    model: string; p_yes_at_entry: number | null; p_yes_at_exit: number | null
    lead_days: number | null; expected_net_edge_bps: number | null
    realized_pnl_usdc: number | null; reason_code: string
  }[]
}

// ── Helpers ───────────────────────────────────────────────────────────────────

function fetchJson<T>(url: string): Promise<T> {
  return fetch(url).then(r => r.json())
}

const pct        = (n: number) => (n * 100).toFixed(1) + '%'
const usd        = (n: number) => (n >= 0 ? '+' : '') + n.toFixed(4) + ' USDC'
const signedPct  = (n: number) => (n >= 0 ? '+' : '') + (n * 100).toFixed(2) + '%'
const shortSlug  = (s: string) => s.replace('btc-updown-15m-', '')

// ── Small components ──────────────────────────────────────────────────────────

function StatCard({ label, value, sub }: { label: string; value: string; sub?: string }) {
  return (
    <div className="stat-card">
      <div className="stat-label">{label}</div>
      <div className="stat-value">{value}</div>
      {sub && <div className="stat-sub">{sub}</div>}
    </div>
  )
}

/// Parse a market slug like "highest-temperature-in-toronto-on-april-15-2026-18corbelow"
/// into a readable string: "≤18°C · Apr 15" or "≥25°C · Apr 15"
function parseMarketDesc(slug: string): string {
  if (!slug) return '—'
  const tempMatch = slug.match(/(\d+)(c|f)or?(above|below)/i)
  if (!tempMatch) return slug.replace(/^highest-temperature-in-/, '').replace(/-/g, ' ')
  const [, rawTemp, unit, dir] = tempMatch
  const tempNum = parseInt(rawTemp)
  const tempStr = unit.toLowerCase() === 'c' ? `${tempNum}°C` : `${tempNum}°F`
  const sign = dir.toLowerCase() === 'below' ? '≤' : '≥'
  const dateMatch = slug.match(/on-(\w+)-(\d+)-(\d{4})/i)
  if (!dateMatch) return `${sign}${tempStr}`
  const [, month, day] = dateMatch
  const months: Record<string, string> = {
    january:'Jan', february:'Feb', march:'Mar', april:'Apr',
    may:'May', june:'Jun', july:'Jul', august:'Aug',
    september:'Sep', october:'Oct', november:'Nov', december:'Dec',
  }
  return `${sign}${tempStr} · ${months[month.toLowerCase()] ?? month} ${day}`
}

function ActionBadge({ action }: { action: string }) {
  // [bg, textColor] pairs — NO_TRADE uses muted style; exits use vivid
  const styles: Record<string, [string, string]> = {
    ENTRY:           ['#052e16', '#34d399'],   // dark green bg, bright green text
    TAKE_PROFIT:     ['#052e16', '#6ee7b7'],
    STOP_LOSS:       ['#2d0a0a', '#f87171'],
    TIME_DECAY_EXIT: ['#431407', '#fb923c'],
    FORECAST_SHIFT:  ['#2e1065', '#c4b5fd'],
    TIME_EXIT:       ['#2d1b00', '#fbbf24'],
    NO_TRADE:        ['#1e2535', '#94a3b8'],   // visible but clearly de-emphasised
  }
  const [bg, fg] = styles[action] ?? ['#1e293b', '#cbd5e1']
  const label = action === 'TIME_DECAY_EXIT' ? 'TD_EXIT'
              : action === 'FORECAST_SHIFT'  ? 'FS_EXIT'
              : action
  return (
    <span style={{
      display: 'inline-block', padding: '2px 7px', borderRadius: 4,
      fontSize: 10, fontWeight: 700, letterSpacing: '0.02em',
      background: bg, color: fg,
      border: `1px solid ${fg}33`,
    }}>{label}</span>
  )
}

function ReasonBadge({ code }: { code: string }) {
  const colors: Record<string, string> = {
    EDGE_OK: '#34d399', EDGE_TOO_LOW: '#f87171', SPREAD_TOO_WIDE: '#fb923c',
    DEPTH_TOO_THIN: '#fbbf24', TIME_EXIT: '#a78bfa', TAKE_PROFIT: '#6ee7b7',
    STOP_LOSS: '#f87171',
  }
  return <span style={{ color: colors[code] ?? '#94a3b8', fontSize: 11 }}>{code}</span>
}

function LadderActionBadge({ action }: { action: string }) {
  const styles: Record<string, [string, string]> = {
    LADDER_ENTRY:           ['#0c1a30', '#60a5fa'],
    HOLD_TO_RESOLUTION:     ['#052e16', '#34d399'],
    CATASTROPHIC_SHIFT_EXIT:['#2d0a0a', '#f87171'],
  }
  const [bg, fg] = styles[action] ?? ['#1e293b', '#cbd5e1']
  const label = action === 'HOLD_TO_RESOLUTION' ? 'HOLD_RES'
              : action === 'CATASTROPHIC_SHIFT_EXIT' ? 'CAT_EXIT'
              : action === 'LADDER_ENTRY' ? 'ENTRY'
              : action
  return (
    <span style={{
      display: 'inline-block', padding: '2px 7px', borderRadius: 4,
      fontSize: 10, fontWeight: 700, letterSpacing: '0.02em',
      background: bg, color: fg, border: `1px solid ${fg}33`,
    }}>{label}</span>
  )
}

function Paginator({
  total, page, pageSize, onPage, onPageSize,
  pageSizeOptions = [10, 25, 50],
}: {
  total: number; page: number; pageSize: number
  onPage: (p: number) => void; onPageSize: (n: number) => void
  pageSizeOptions?: number[]
}) {
  const totalPages = Math.max(1, Math.ceil(total / pageSize))
  const btnStyle = (disabled: boolean): React.CSSProperties => ({
    background: disabled ? 'transparent' : '#1e1e42',
    border: '1px solid #2e3348', borderRadius: 4,
    color: disabled ? '#3a3a58' : '#a5b4fc',
    cursor: disabled ? 'default' : 'pointer',
    fontSize: 11, padding: '2px 8px', lineHeight: '18px',
  })
  return (
    <div style={{
      display: 'flex', alignItems: 'center', gap: 6,
      padding: '6px 0 2px', fontSize: 11, color: '#8892a4',
    }}
      onClick={e => e.stopPropagation()}
    >
      <button style={btnStyle(page === 0)} disabled={page === 0}
        onClick={() => onPage(0)}>«</button>
      <button style={btnStyle(page === 0)} disabled={page === 0}
        onClick={() => onPage(page - 1)}>‹</button>
      <span style={{ minWidth: 60, textAlign: 'center', color: '#cbd5e1' }}>
        {page + 1} / {totalPages}
      </span>
      <button style={btnStyle(page >= totalPages - 1)} disabled={page >= totalPages - 1}
        onClick={() => onPage(page + 1)}>›</button>
      <button style={btnStyle(page >= totalPages - 1)} disabled={page >= totalPages - 1}
        onClick={() => onPage(totalPages - 1)}>»</button>
      <select
        value={pageSize}
        onChange={e => { onPageSize(Number(e.target.value)); onPage(0) }}
        style={{
          background: '#0e0e1a', border: '1px solid #2e3348', borderRadius: 4,
          color: '#8892a4', fontSize: 11, padding: '2px 4px', marginLeft: 6,
        }}
      >
        {pageSizeOptions.map(n => (
          <option key={n} value={n}>{n} 筆/頁</option>
        ))}
      </select>
      <span style={{ color: '#4b5563', marginLeft: 2 }}>共 {total} 筆</span>
    </div>
  )
}

// ── BTC 15m tab ───────────────────────────────────────────────────────────────

function BtcTab() {
  const [stats, setStats] = useState<Stats | null>(null)
  const [strategies, setStrategies] = useState<StrategySummary[]>([])
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [detail, setDetail] = useState<StrategyDetail | null>(null)
  const [cycles, setCycles] = useState<Cycle[]>([])
  const [dryRuns, setDryRuns] = useState<DryRun[]>([])
  const [history, setHistory] = useState<DailyWinRate[]>([])
  const [tick, setTick] = useState(0)

  useEffect(() => {
    const timer = setInterval(() => setTick(t => t + 1), 5000)
    return () => clearInterval(timer)
  }, [])

  useEffect(() => {
    fetchJson<Stats>('/api/stats').then(setStats)
    fetchJson<Cycle[]>('/api/cycles').then(setCycles)
    fetchJson<DryRun[]>('/api/dry-runs').then(setDryRuns)
    fetchJson<DailyWinRate[]>('/api/winrate-history').then(setHistory)
    fetchJson<StrategySummary[]>('/api/strategies?days=7').then(rows => {
      setStrategies(rows)
      setSelectedId(prev => prev ?? (rows.length > 0 ? rows[0].strategy_id : null))
    })
  }, [tick])

  useEffect(() => {
    if (!selectedId) { setDetail(null); return }
    fetchJson<StrategyDetail>(
      `/api/strategy-detail?strategy_id=${encodeURIComponent(selectedId)}&days=30`
    ).then(setDetail)
  }, [selectedId, tick])

  return (
    <>
      {/* Stat cards */}
      <section className="cards">
        <StatCard label="Today's Cycles" value={stats ? String(stats.total_cycles) : '—'}
          sub={stats ? `${stats.triggered_cycles} triggered` : undefined} />
        <StatCard label="Trigger Rate" value={stats ? pct(stats.trigger_rate) : '—'}
          sub="leg 1 fired / total" />
        <StatCard label="Win Rate"
          value={stats && stats.triggered_cycles > 0 ? pct(stats.win_rate) : '—'}
          sub="resolved cycles" />
        <StatCard label="Est. PnL (today)" value={stats ? usd(stats.total_pnl_usdc) : '—'}
          sub={stats ? `as of ${new Date(stats.as_of).toLocaleTimeString()}` : undefined} />
      </section>

      {/* Strategy performance table */}
      <section className="panel">
        <h2>Strategy Performance (7d)</h2>
        {strategies.length === 0
          ? <p className="empty">No strategy data yet.</p>
          : (
            <div className="table-wrap">
              <table>
                <thead><tr>
                  <th>Strategy</th><th>Cycles</th><th>Trades</th>
                  <th>Win Rate</th><th>PnL</th><th>PnL % (alloc)</th><th>Action</th>
                </tr></thead>
                <tbody>
                  {strategies.map(s => (
                    <tr key={s.strategy_id}
                      className={selectedId === s.strategy_id ? 'row-selected' : ''}>
                      <td className="slug">{s.strategy_id}</td>
                      <td>{s.total_cycles}</td>
                      <td>{s.total_trades}</td>
                      <td>{s.total_cycles > 0 ? pct(s.win_rate) : '—'}</td>
                      <td className={s.total_pnl_usdc >= 0 ? 'green' : 'red'}>{usd(s.total_pnl_usdc)}</td>
                      <td className={s.pnl_pct_alloc >= 0 ? 'green' : 'red'}>{signedPct(s.pnl_pct_alloc)}</td>
                      <td>
                        <button className="btn-link" onClick={() => setSelectedId(s.strategy_id)}>
                          Details
                        </button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
      </section>

      {/* Strategy detail */}
      <section className="panel">
        <h2>Strategy Detail{detail ? ` — ${detail.strategy_id}` : ''}</h2>
        {!detail
          ? <p className="empty">Select a strategy above to view details.</p>
          : (
            <>
              <section className="cards detail-cards">
                <StatCard label="Initial Allocated"
                  value={usd(detail.initial_allocated_usdc)}
                  sub={signedPct(detail.capital_allocation_pct)} />
                <StatCard label="Total Invested (30d)"
                  value={usd(detail.total_invested_usdc)}
                  sub={`${detail.total_trades} trades`} />
                <StatCard label="PnL (30d)"
                  value={usd(detail.total_pnl_usdc)}
                  sub={`ROI alloc ${signedPct(detail.pnl_pct_alloc)}`} />
                <StatCard label="Win Rate"
                  value={pct(detail.win_rate)}
                  sub={`${detail.triggered_cycles} triggered / ${detail.total_cycles} cycles`} />
              </section>
              <div className="detail-grid">
                <div>
                  <h3>Recent Trades</h3>
                  {detail.recent_trades.length === 0
                    ? <p className="empty">No trades yet.</p>
                    : (
                      <div className="table-wrap">
                        <table>
                          <thead><tr>
                            <th>Time</th><th>Leg</th><th>Side</th>
                            <th>Price</th><th>Size</th><th>Fee</th>
                          </tr></thead>
                          <tbody>
                            {detail.recent_trades.map(t => (
                              <tr key={t.id}>
                                <td className="ts">{t.ts?.slice(0, 19) ?? '—'}</td>
                                <td>{t.leg}</td><td>{t.side}</td>
                                <td>{t.price.toFixed(4)}</td>
                                <td>{t.size_usdc.toFixed(2)}</td>
                                <td>{t.fee_usdc.toFixed(4)}</td>
                              </tr>
                            ))}
                          </tbody>
                        </table>
                      </div>
                    )}
                </div>
                <div>
                  <h3>Recent Cycles</h3>
                  {detail.recent_cycles.length === 0
                    ? <p className="empty">No cycles yet.</p>
                    : (
                      <div className="table-wrap">
                        <table>
                          <thead><tr>
                            <th>Created</th><th>Leg1</th><th>Leg2</th><th>PnL</th>
                          </tr></thead>
                          <tbody>
                            {detail.recent_cycles.slice(0, 5).map(c => (
                              <tr key={c.id}>
                                <td className="ts">{c.created_at?.slice(0, 19) ?? '—'}</td>
                                <td>{c.leg1_price != null ? c.leg1_price.toFixed(4) : '—'}</td>
                                <td>{c.leg2_price != null ? c.leg2_price.toFixed(4) : '—'}</td>
                                <td className={c.pnl_usdc != null ? (c.pnl_usdc >= 0 ? 'green' : 'red') : ''}>
                                  {c.pnl_usdc != null ? usd(c.pnl_usdc) : '—'}
                                </td>
                              </tr>
                            ))}
                          </tbody>
                        </table>
                      </div>
                    )}
                </div>
              </div>
            </>
          )}
      </section>

      {/* Win-rate chart */}
      <section className="panel">
        <h2>Win Rate — Last 14 Days</h2>
        {history.length === 0
          ? <p className="empty">No resolved cycles yet.</p>
          : (
            <ResponsiveContainer width="100%" height={200}>
              <LineChart data={history} margin={{ top: 8, right: 16, bottom: 8, left: 0 }}>
                <CartesianGrid strokeDasharray="3 3" stroke="#2a2a3a" />
                <XAxis dataKey="date" tick={{ fontSize: 11 }} />
                <YAxis domain={[0, 1]} tickFormatter={pct} tick={{ fontSize: 11 }} />
                <Tooltip formatter={(v) => typeof v === 'number' ? pct(v) : v} />
                <Line type="monotone" dataKey="win_rate" stroke="#6ee7b7" strokeWidth={2} dot={{ r: 3 }} />
              </LineChart>
            </ResponsiveContainer>
          )}
      </section>

      {/* Cycles table */}
      <section className="panel">
        <h2>Recent Market Cycles <span className="count">({cycles.length})</span></h2>
        {cycles.length === 0
          ? <p className="empty">No cycles recorded yet.</p>
          : (
            <div className="table-wrap">
              <table>
                <thead><tr>
                  <th>Slug</th><th>Leg 1</th><th>Leg1 Price</th>
                  <th>Leg2 Price</th><th>Est. PnL</th><th>Created</th>
                </tr></thead>
                <tbody>
                  {cycles.slice(0, 15).map(c => (
                    <tr key={c.id}
                      className={c.pnl_usdc != null ? (c.pnl_usdc >= 0 ? 'row-win' : 'row-loss') : ''}>
                      <td className="slug">{shortSlug(c.market_slug)}</td>
                      <td>{c.leg1_triggered ? '✓' : '—'}</td>
                      <td>{c.leg1_price != null ? c.leg1_price.toFixed(4) : '—'}</td>
                      <td>{c.leg2_price != null ? c.leg2_price.toFixed(4) : '—'}</td>
                      <td className={c.pnl_usdc != null ? (c.pnl_usdc >= 0 ? 'green' : 'red') : ''}>
                        {c.pnl_usdc != null ? usd(c.pnl_usdc) : '—'}
                      </td>
                      <td className="ts">{c.created_at?.slice(0, 19) ?? '—'}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
      </section>

      {/* Dry-run trades */}
      <section className="panel">
        <h2>Dry-Run Trades <span className="count">({dryRuns.length})</span></h2>
        {dryRuns.length === 0
          ? <p className="empty">No trades recorded yet.</p>
          : (
            <div className="table-wrap">
              <table>
                <thead><tr>
                  <th>Slug</th><th>Leg</th><th>Side</th><th>Price</th>
                  <th>Size</th><th>Dump %</th><th>Hedge Sum</th><th>Time</th>
                </tr></thead>
                <tbody>
                  {dryRuns.map(t => (
                    <tr key={t.id}>
                      <td className="slug">{shortSlug(t.market_slug)}</td>
                      <td>{t.leg}</td><td>{t.side}</td>
                      <td>{t.price.toFixed(4)}</td>
                      <td>{t.size_usdc.toFixed(0)}</td>
                      <td>{t.signal_dump_pct != null ? pct(t.signal_dump_pct) : '—'}</td>
                      <td>{t.hedge_sum != null ? t.hedge_sum.toFixed(4) : '—'}</td>
                      <td className="ts">{t.ts?.slice(0, 19) ?? '—'}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
      </section>
    </>
  )
}

// ── Mention Market tab ────────────────────────────────────────────────────────

function MentionTab() {
  const [mentionStats, setMentionStats] = useState<MentionStats | null>(null)
  const [strategies, setStrategies] = useState<MentionStrategySummary[]>([])
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [detail, setDetail] = useState<MentionStrategyDetail | null>(null)
  const [tick, setTick] = useState(0)

  useEffect(() => {
    const timer = setInterval(() => setTick(t => t + 1), 5000)
    return () => clearInterval(timer)
  }, [])

  useEffect(() => {
    fetchJson<MentionStats>('/api/mention/stats?days=7').then(setMentionStats)
    fetchJson<MentionStrategySummary[]>('/api/mention/strategies?days=7').then(rows => {
      setStrategies(rows)
      setSelectedId(prev => prev ?? (rows.length > 0 ? rows[0].strategy_id : null))
    })
  }, [tick])

  useEffect(() => {
    if (!selectedId) { setDetail(null); return }
    fetchJson<MentionStrategyDetail>(
      `/api/mention/strategy-detail?strategy_id=${encodeURIComponent(selectedId)}&days=30`
    ).then(setDetail)
  }, [selectedId, tick])

  const total = mentionStats?.total_rows ?? 0

  return (
    <>
      {/* Aggregate stat cards */}
      <section className="cards">
        <StatCard
          label="Entries (7d)"
          value={mentionStats ? String(mentionStats.entry_count) : '—'}
          sub={mentionStats ? `${mentionStats.cancel_count} cancelled` : undefined}
        />
        <StatCard
          label="Trigger Rate"
          value={mentionStats ? pct(mentionStats.trigger_rate) : '—'}
          sub="entries / signals"
        />
        <StatCard
          label="Fill Rate"
          value={mentionStats ? pct(mentionStats.fill_rate) : '—'}
          sub="TP+SL / entries"
        />
        <StatCard
          label="Net PnL (7d)"
          value={mentionStats ? usd(mentionStats.net_pnl_usdc) : '—'}
          sub={mentionStats?.avg_hold_sec != null
            ? `avg hold ${Math.round(mentionStats.avg_hold_sec)}s`
            : 'no exits yet'}
        />
        <StatCard
          label="Max Drawdown"
          value={mentionStats ? pct(mentionStats.max_drawdown_pct) : '—'}
          sub={mentionStats ? `${mentionStats.max_consecutive_losses} consec. losses` : undefined}
        />
        <StatCard
          label="Sharpe (ann.)"
          value={mentionStats && mentionStats.entry_count >= 2
            ? mentionStats.sharpe_ratio.toFixed(3) : '—'}
          sub="annualised"
        />
      </section>

      {/* Strategy performance table — same format as BTC tab */}
      <section className="panel">
        <h2>Strategy Performance (7d)</h2>
        {strategies.length === 0
          ? <p className="empty">No mention strategy data yet — start the engine with a mention strategy enabled.</p>
          : (
            <div className="table-wrap">
              <table>
                <thead><tr>
                  <th>Strategy</th><th>Entries</th><th>Trigger Rate</th>
                  <th>Fill Rate</th><th>Net PnL</th><th>PnL % (alloc)</th>
                  <th>Markets</th><th>Action</th>
                </tr></thead>
                <tbody>
                  {strategies.map(s => (
                    <tr key={s.strategy_id}
                      className={selectedId === s.strategy_id ? 'row-selected' : ''}>
                      <td className="slug">{s.strategy_id}</td>
                      <td>{s.entry_count}</td>
                      <td>{s.entry_count + s.cancel_count > 0 ? pct(s.trigger_rate) : '—'}</td>
                      <td>{s.entry_count > 0 ? pct(s.fill_rate) : '—'}</td>
                      <td className={s.net_pnl_usdc >= 0 ? 'green' : 'red'}>{usd(s.net_pnl_usdc)}</td>
                      <td className={s.pnl_pct_alloc >= 0 ? 'green' : 'red'}>{signedPct(s.pnl_pct_alloc)}</td>
                      <td>{s.total_markets}</td>
                      <td>
                        <button className="btn-link" onClick={() => setSelectedId(s.strategy_id)}>
                          Details
                        </button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
      </section>

      {/* Strategy detail — same structure as BTC tab */}
      <section className="panel">
        <h2>Strategy Detail{detail ? ` — ${detail.strategy_id}` : ''}</h2>
        {!detail
          ? <p className="empty">Select a strategy above to view details.</p>
          : (
            <>
              <section className="cards detail-cards">
                <StatCard
                  label="Initial Allocated"
                  value={usd(detail.initial_allocated_usdc)}
                  sub={signedPct(detail.capital_allocation_pct)}
                />
                <StatCard
                  label="Entries (30d)"
                  value={String(detail.entry_count)}
                  sub={`${detail.total_signals} signals`}
                />
                <StatCard
                  label="Net PnL (30d)"
                  value={usd(detail.net_pnl_usdc)}
                  sub={`ROI alloc ${signedPct(detail.pnl_pct_alloc)}`}
                />
                <StatCard
                  label="Win Rate"
                  value={pct(detail.win_rate)}
                  sub={`TP ${detail.tp_count} / SL ${detail.sl_count} / TE ${detail.te_count}`}
                />
              </section>

              <div className="detail-grid">
                {/* Left: recent trades */}
                <div>
                  <h3>Recent Trades</h3>
                  {detail.recent_trades.length === 0
                    ? <p className="empty">No trades yet.</p>
                    : (
                      <div className="table-wrap">
                        <table>
                          <thead><tr>
                            <th>Time</th><th>Keyword</th><th>Side</th>
                            <th>Action</th><th>Price</th><th>Hold (s)</th>
                            <th>PnL</th><th>Reason</th>
                          </tr></thead>
                          <tbody>
                            {detail.recent_trades.map(t => (
                              <tr key={t.id}
                                className={t.realized_pnl_usdc != null
                                  ? (t.realized_pnl_usdc >= 0 ? 'row-win' : 'row-loss') : ''}>
                                <td className="ts">{t.ts?.slice(0, 19) ?? '—'}</td>
                                <td style={{ color: '#a5b4fc', fontFamily: 'monospace', fontSize: 11 }}>
                                  {t.keyword}
                                </td>
                                <td>
                                  <span style={{
                                    color: t.side === 'NO' ? '#fb923c' : '#6ee7b7',
                                    fontWeight: 600, fontSize: 11,
                                  }}>{t.side}</span>
                                </td>
                                <td><ActionBadge action={t.action} /></td>
                                <td>{t.price.toFixed(4)}</td>
                                <td>{t.hold_sec != null ? t.hold_sec : '—'}</td>
                                <td className={t.realized_pnl_usdc != null
                                  ? (t.realized_pnl_usdc >= 0 ? 'green' : 'red') : ''}>
                                  {t.realized_pnl_usdc != null ? usd(t.realized_pnl_usdc) : '—'}
                                </td>
                                <td><ReasonBadge code={t.reason_code} /></td>
                              </tr>
                            ))}
                          </tbody>
                        </table>
                      </div>
                    )}
                </div>

                {/* Right: keywords & markets */}
                <div>
                  <h3>Active Keywords</h3>
                  {detail.active_keywords.length === 0
                    ? <p className="empty">No keyword matches yet.</p>
                    : (
                      <div className="tag-list">
                        {detail.active_keywords.map(kw => (
                          <span key={kw} className="tag tag-keyword">{kw}</span>
                        ))}
                      </div>
                    )}
                  <h3 style={{ marginTop: 16 }}>Active Markets</h3>
                  {detail.active_markets.length === 0
                    ? <p className="empty">No market entries yet.</p>
                    : (
                      <div className="tag-list">
                        {detail.active_markets.map(m => (
                          <span key={m} className="tag tag-market">{m}</span>
                        ))}
                      </div>
                    )}
                  {detail.avg_hold_sec != null && (
                    <div style={{ marginTop: 16, color: '#64748b', fontSize: 12 }}>
                      Avg hold: {Math.round(detail.avg_hold_sec)}s
                      {detail.avg_entry_price != null && (
                        <> &nbsp;·&nbsp; Avg entry: {detail.avg_entry_price.toFixed(4)}</>
                      )}
                      {detail.avg_exit_price != null && (
                        <> &nbsp;·&nbsp; Avg exit: {detail.avg_exit_price.toFixed(4)}</>
                      )}
                    </div>
                  )}
                </div>
              </div>
            </>
          )}
      </section>

      {/* Signal rejection breakdown */}
      {total > 0 && (
        <section className="panel">
          <h2>Signal Rejection Breakdown (7d)</h2>
          <div className="rejection-grid">
            {([
              ['Edge Too Low',    mentionStats!.edge_too_low_count,    '#f87171'],
              ['Spread Too Wide', mentionStats!.spread_too_wide_count, '#fb923c'],
              ['Depth Too Thin',  mentionStats!.depth_too_thin_count,  '#fbbf24'],
            ] as [string, number, string][]).map(([label, count, color]) => (
              <div key={label} className="rejection-card">
                <div className="rejection-label">{label}</div>
                <div className="rejection-value" style={{ color }}>{count}</div>
                <div className="rejection-bar">
                  <div className="rejection-fill"
                    style={{ width: `${Math.min(100, (count / total) * 100)}%`, background: color }} />
                </div>
              </div>
            ))}
          </div>
        </section>
      )}
    </>
  )
}

// ── Weather Market tab ────────────────────────────────────────────────────────

function WeatherTab() {
  const [weatherStats, setWeatherStats] = useState<WeatherStats | null>(null)
  const [strategies, setStrategies] = useState<WeatherStrategySummary[]>([])
  const [ladderStrategies, setLadderStrategies] = useState<WeatherLadderStrategySummary[]>([])
  const [tick, setTick] = useState(0)
  const [expandedId, setExpandedId] = useState<string | null>(null)
  const [ladderExpandedId, setLadderExpandedId] = useState<string | null>(null)
  const [detailCache, setDetailCache] = useState<Record<string, WeatherStrategyDetail>>({})
  const [ladderDetailCache, setLadderDetailCache] = useState<Record<string, WeatherLadderStrategyDetail>>({})
  const [tradeFilters, setTradeFilters] = useState<Record<string, string>>({})
  const [ladderFilterMap, setLadderFilterMap] = useState<Record<string, string>>({})
  const [pageMap, setPageMap] = useState<Record<string, number>>({})
  const [pageSizeMap, setPageSizeMap] = useState<Record<string, number>>({})

  useEffect(() => {
    const timer = setInterval(() => setTick(t => t + 1), 5000)
    return () => clearInterval(timer)
  }, [])

  useEffect(() => {
    fetchJson<WeatherStats>('/api/weather/stats?days=7').then(setWeatherStats)
    fetchJson<WeatherStrategySummary[]>('/api/weather/strategies?days=7').then(setStrategies)
    fetchJson<WeatherLadderStrategySummary[]>('/api/ladder/strategies?days=7').then(setLadderStrategies)
  }, [tick])

  useEffect(() => {
    if (!expandedId) return
    fetchJson<WeatherStrategyDetail>(
      `/api/weather/strategy-detail?strategy_id=${encodeURIComponent(expandedId)}&days=30`
    ).then(d => setDetailCache(prev => ({ ...prev, [expandedId]: d })))
  }, [expandedId, tick])

  useEffect(() => {
    if (!ladderExpandedId) return
    fetchJson<WeatherLadderStrategyDetail>(
      `/api/ladder/strategy-detail?strategy_id=${encodeURIComponent(ladderExpandedId)}&days=30`
    ).then(d => setLadderDetailCache(prev => ({ ...prev, [ladderExpandedId]: d })))
  }, [ladderExpandedId, tick])

  const toggleExpand = (id: string) =>
    setExpandedId(prev => prev === id ? null : id)

  const exits = weatherStats?.exits
  const exitTotal = exits
    ? exits.take_profit + exits.stop_loss + exits.forecast_shift + exits.time_decay_exit
    : 0

  return (
    <>
      {/* Aggregate stat cards */}
      <section className="cards">
        <StatCard
          label="Entries (7d)"
          value={weatherStats ? String(weatherStats.entry_count) : '—'}
          sub={weatherStats ? `${weatherStats.cancel_count} cancelled` : undefined}
        />
        <StatCard
          label="Trigger Rate"
          value={weatherStats ? pct(weatherStats.trigger_rate) : '—'}
          sub="entries / signals"
        />
        <StatCard
          label="Fill Rate"
          value={weatherStats ? pct(weatherStats.fill_rate) : '—'}
          sub="TP+SL+FS / entries"
        />
        <StatCard
          label="Net PnL (7d)"
          value={weatherStats ? usd(weatherStats.net_pnl_usdc) : '—'}
          sub={weatherStats?.avg_hold_sec != null
            ? `avg hold ${Math.round(weatherStats.avg_hold_sec)}s`
            : 'no exits yet'}
        />
        <StatCard
          label="Win Rate"
          value={weatherStats && weatherStats.entry_count > 0
            ? pct(weatherStats.win_rate_overall) : '—'}
          sub={weatherStats ? `${weatherStats.max_consecutive_losses} consec. losses` : undefined}
        />
        <StatCard
          label="Sharpe (ann.)"
          value={weatherStats && weatherStats.entry_count >= 2
            ? weatherStats.sharpe_ratio.toFixed(3) : '—'}
          sub="annualised"
        />
      </section>

      {/* Strategy performance table — rows are expandable */}
      <section className="panel">
        <h2>Strategy Performance (7d)</h2>
        {strategies.length === 0
          ? <p className="empty">No weather strategy data yet — start the engine with a weather strategy enabled.</p>
          : (
            <div className="table-wrap">
              <table>
                <thead><tr>
                  <th>Strategy</th><th>Entries</th><th>Trigger Rate</th>
                  <th>Fill Rate</th><th>Net PnL</th><th>PnL % (alloc)</th>
                  <th>Cities</th>
                </tr></thead>
                <tbody>
                  {strategies.map(s => {
                    const isExpanded = expandedId === s.strategy_id
                    const cached = detailCache[s.strategy_id]

                    // Trade grouping — computed once, referenced in expanded row
                    const allTrades     = cached?.recent_trades ?? []
                    // open_positions = truly open (no exit yet); used for OPEN box + city dots
                    const openPositions = cached?.open_positions ?? []
                    // entryTrades = all ENTRY records in the trade log (open + closed history)
                    const entryTrades   = allTrades.filter(t => t.action === 'ENTRY')
                    const exitTrades    = allTrades.filter(t =>
                      ['TAKE_PROFIT','STOP_LOSS','FORECAST_SHIFT','TIME_DECAY_EXIT'].includes(t.action))
                    const noTradeTrades = allTrades.filter(t => t.action === 'NO_TRADE')
                    type TF = 'all' | 'entry' | 'exit' | 'no_trade'
                    const activeFilter  = (tradeFilters[s.strategy_id] ?? 'all') as TF
                    const filteredTrades =
                      activeFilter === 'entry'    ? entryTrades :
                      activeFilter === 'exit'     ? exitTrades :
                      activeFilter === 'no_trade' ? noTradeTrades :
                      allTrades
                    const page     = pageMap[s.strategy_id] ?? 0
                    const pageSize = pageSizeMap[s.strategy_id] ?? 10
                    const shownTrades = filteredTrades.slice(page * pageSize, (page + 1) * pageSize)

                    return (
                      <Fragment key={s.strategy_id}>
                        {/* ── Summary row ─────────────────────────────────── */}
                        <tr
                          onClick={() => toggleExpand(s.strategy_id)}
                          style={{ cursor: 'pointer' }}
                          className={isExpanded ? 'row-selected' : ''}
                        >
                          <td className="slug">
                            <span style={{ marginRight: 6, opacity: 0.5, fontSize: 10, userSelect: 'none' }}>
                              {isExpanded ? '▼' : '▶'}
                            </span>
                            {s.strategy_id}
                            {/* Live-position badge visible even when collapsed */}
                            {openPositions.length > 0 && (
                              <span style={{
                                marginLeft: 8,
                                display: 'inline-flex', alignItems: 'center', gap: 4,
                                background: 'rgba(52,211,153,0.12)',
                                border: '1px solid rgba(52,211,153,0.35)',
                                borderRadius: 10, padding: '1px 7px',
                                fontSize: 9, fontWeight: 700, color: '#34d399',
                              }}>
                                <span style={{
                                  width: 5, height: 5, borderRadius: '50%',
                                  background: '#34d399', boxShadow: '0 0 4px #34d399',
                                  display: 'inline-block',
                                }} />
                                {openPositions.length}
                              </span>
                            )}
                          </td>
                          <td>{s.entry_count}</td>
                          <td>{s.entry_count + s.cancel_count > 0 ? pct(s.trigger_rate) : '—'}</td>
                          <td>{s.entry_count > 0 ? pct(s.fill_rate) : '—'}</td>
                          <td className={s.net_pnl_usdc >= 0 ? 'green' : 'red'}>{usd(s.net_pnl_usdc)}</td>
                          <td className={s.pnl_pct_alloc >= 0 ? 'green' : 'red'}>{signedPct(s.pnl_pct_alloc)}</td>
                          <td>{s.total_cities}</td>
                        </tr>

                        {/* ── Expanded detail row ──────────────────────────── */}
                        {isExpanded && (
                          <tr>
                            <td colSpan={7} style={{ padding: 0, background: '#080812', borderBottom: '1px solid #1a1a2e' }}>
                              <div style={{ padding: '14px 18px 16px' }}>

                                {/* ── Active entries list ── */}
                                {openPositions.length > 0 && (
                                  <div style={{
                                    background: 'rgba(52,211,153,0.04)',
                                    border: '1px solid rgba(52,211,153,0.22)',
                                    borderRadius: 6, marginBottom: 12, overflow: 'hidden',
                                  }}>
                                    {/* header */}
                                    <div style={{
                                      display: 'flex', alignItems: 'center', gap: 8,
                                      padding: '6px 12px',
                                      borderBottom: '1px solid rgba(52,211,153,0.15)',
                                      background: 'rgba(52,211,153,0.06)',
                                    }}>
                                      <span style={{ width: 7, height: 7, borderRadius: '50%', flexShrink: 0,
                                        background: '#34d399', boxShadow: '0 0 5px #34d399', display: 'inline-block' }} />
                                      <span style={{ fontSize: 11, fontWeight: 700, color: '#34d399', letterSpacing: '0.06em' }}>
                                        OPEN {openPositions.length}
                                      </span>
                                      <span style={{ fontSize: 10, color: '#64748b', marginLeft: 4 }}>— 目前持倉</span>
                                    </div>
                                    {/* rows */}
                                    <div style={{ display: 'grid',
                                      gridTemplateColumns: 'min-content min-content 5.5rem 1fr min-content min-content min-content',
                                      alignItems: 'center', gap: '0 14px',
                                      padding: '0 12px',
                                    }}>
                                      {/* col headers */}
                                      {['CITY','SIDE','@PRICE','MARKET','P_YES','LEAD','TIME'].map(h => (
                                        <span key={h} style={{ fontSize: 9, color: '#4b5563', fontWeight: 700,
                                          letterSpacing: '0.08em', padding: '5px 0 3px' }}>{h}</span>
                                      ))}
                                      {openPositions.map(t => (
                                        <Fragment key={t.id}>
                                          <span style={{ fontFamily: 'monospace', fontSize: 11, fontWeight: 600,
                                            color: '#6ee7b7', paddingBottom: 5 }}>{t.city}</span>
                                          <span style={{ fontSize: 11, fontWeight: 700, paddingBottom: 5,
                                            color: t.side === 'NO' ? '#fb923c' : '#34d399' }}>{t.side}</span>
                                          <span style={{ fontSize: 11, fontWeight: 600, color: '#e2e8f0', paddingBottom: 5 }}>
                                            {t.price.toFixed(4)}
                                          </span>
                                          <span style={{ fontSize: 10, color: '#a5b4fc', paddingBottom: 5,
                                            overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap',
                                            fontFamily: 'monospace' }}>
                                            {parseMarketDesc(t.market_slug ?? '')}
                                          </span>
                                          <span style={{ fontSize: 11, color: '#94a3b8', paddingBottom: 5 }}>
                                            {t.p_yes_at_entry != null ? t.p_yes_at_entry.toFixed(3) : '—'}
                                          </span>
                                          <span style={{ fontSize: 11, color: '#8892a4', paddingBottom: 5 }}>
                                            {t.lead_days != null ? `${t.lead_days}d` : '—'}
                                          </span>
                                          <span style={{ fontSize: 10, color: '#64748b', fontFamily: 'monospace', paddingBottom: 5 }}>
                                            {t.ts?.slice(11, 19)}
                                          </span>
                                        </Fragment>
                                      ))}
                                    </div>
                                  </div>
                                )}

                                {/* ── Cities + stats row ── */}
                                <div style={{
                                  display: 'flex', justifyContent: 'space-between',
                                  alignItems: 'flex-start', marginBottom: 10,
                                  gap: 12, flexWrap: 'wrap',
                                }}>
                                  {/* City chips — green dot if currently active */}
                                  <div style={{ display: 'flex', flexWrap: 'wrap', gap: 5, alignItems: 'center' }}>
                                    <span style={{ fontSize: 11, color: '#64748b', marginRight: 2 }}>Cities</span>
                                    {s.active_cities.length === 0
                                      ? <span style={{ fontSize: 11, color: '#64748b' }}>—</span>
                                      : s.active_cities.map(c => {
                                          const live = openPositions.some(e => e.city === c)
                                          return (
                                            <span key={c} style={{
                                              display: 'inline-flex', alignItems: 'center', gap: 4,
                                              background: live ? '#091c12' : '#141428',
                                              border: `1px solid ${live ? '#166534' : '#252540'}`,
                                              borderRadius: 5, padding: '2px 7px',
                                              fontSize: 11, fontFamily: 'monospace',
                                              color: live ? '#6ee7b7' : '#7c8db5',
                                            }}>
                                              {live && (
                                                <span style={{
                                                  width: 5, height: 5, borderRadius: '50%',
                                                  background: '#34d399', display: 'inline-block',
                                                }} />
                                              )}
                                              {c}
                                            </span>
                                          )
                                        })
                                    }
                                    {s.total_cities > s.active_cities.length && (
                                      <span style={{ fontSize: 10, color: '#64748b' }}>
                                        +{s.total_cities - s.active_cities.length} more
                                      </span>
                                    )}
                                  </div>

                                  {/* Compact stats strip */}
                                  {cached ? (
                                    <div style={{ display: 'flex', gap: 12, fontSize: 11, color: '#8892a4', flexWrap: 'wrap', alignItems: 'center' }}>
                                      <span><span style={{ color: '#64748b' }}>Alloc </span>
                                        <strong style={{ color: '#e2e8f0' }}>{cached.initial_allocated_usdc.toFixed(0)} USDC</strong></span>
                                      <span><span style={{ color: '#64748b' }}>Win </span>
                                        <strong style={{ color: cached.win_rate > 0.5 ? '#34d399' : '#94a3b8' }}>{pct(cached.win_rate)}</strong></span>
                                      <span>
                                        TP <strong style={{ color: '#34d399' }}>{cached.tp_count}</strong>
                                        {' '}· SL <strong style={{ color: '#f87171' }}>{cached.sl_count}</strong>
                                        {' '}· FS <strong style={{ color: '#a78bfa' }}>{cached.fs_count}</strong>
                                        {' '}· TD <strong style={{ color: '#fb923c' }}>{cached.td_count}</strong>
                                      </span>
                                      {cached.avg_entry_price != null && (
                                        <span><span style={{ color: '#64748b' }}>entry </span>
                                          <strong style={{ color: '#e2e8f0' }}>{cached.avg_entry_price.toFixed(4)}</strong></span>
                                      )}
                                      {cached.avg_hold_sec != null && (
                                        <span><span style={{ color: '#64748b' }}>hold </span>
                                          <strong style={{ color: '#e2e8f0' }}>{Math.round(cached.avg_hold_sec)}s</strong></span>
                                      )}
                                    </div>
                                  ) : (
                                    <span style={{ fontSize: 11, color: '#64748b' }}>Loading…</span>
                                  )}
                                </div>

                                {/* ── Filter tabs + trade log ── */}
                                {allTrades.length > 0 && (
                                  <>
                                    <div style={{
                                      display: 'flex', gap: 4, marginBottom: 8,
                                      borderTop: '1px solid #141428', paddingTop: 10,
                                    }}>
                                      {([
                                        ['all',      'All',      allTrades.length,     '#94a3b8'],
                                        ['entry',    'Open',     entryTrades.length,   '#34d399'],
                                        ['exit',     'Closed',   exitTrades.length,    '#a78bfa'],
                                        ['no_trade', 'No Trade', noTradeTrades.length, '#64748b'],
                                      ] as [string, string, number, string][]).map(([f, label, count, color]) => (
                                        <button key={f}
                                          onClick={e => {
                                            e.stopPropagation()
                                            setTradeFilters(prev => ({ ...prev, [s.strategy_id]: f }))
                                          }}
                                          style={{
                                            background: activeFilter === f ? '#1e1e42' : 'transparent',
                                            border: `1px solid ${activeFilter === f ? '#818cf8' : '#2e3348'}`,
                                            borderRadius: 4, padding: '3px 10px', cursor: 'pointer',
                                            fontSize: 11, fontWeight: 600,
                                            color: activeFilter === f ? '#e2e8f0' : '#8892a4',
                                            display: 'inline-flex', alignItems: 'center', gap: 5,
                                          }}
                                        >
                                          {label}
                                          {count > 0 && (
                                            <span style={{
                                              background: '#0e0e1a', borderRadius: 8,
                                              padding: '0 5px', fontSize: 10, color,
                                            }}>{count}</span>
                                          )}
                                        </button>
                                      ))}
                                    </div>

                                    {filteredTrades.length === 0 ? (
                                      <p style={{ color: '#8892a4', fontSize: 11, padding: '4px 0' }}>— 無紀錄</p>
                                    ) : (
                                      <>
                                        <div className="table-wrap">
                                          <table>
                                            <thead><tr>
                                              <th>Time</th><th>City</th><th>Side</th>
                                              <th>Action</th><th>Market</th><th>Model</th><th>p_yes</th>
                                              <th>Lead</th><th>Hold</th><th>PnL</th>
                                            </tr></thead>
                                            <tbody>
                                              {shownTrades.map(t => {
                                                const isEntry   = t.action === 'ENTRY'
                                                const isNoTrade = t.action === 'NO_TRADE'
                                                return (
                                                  <tr key={t.id}
                                                    style={{ opacity: isNoTrade ? 0.55 : 1 }}
                                                    className={
                                                      isEntry ? 'row-win'
                                                      : t.realized_pnl_usdc != null
                                                        ? (t.realized_pnl_usdc >= 0 ? 'row-win' : 'row-loss')
                                                        : ''
                                                    }
                                                  >
                                                    <td className="ts">{t.ts?.slice(11, 19) ?? '—'}</td>
                                                    <td style={{ fontFamily: 'monospace', fontSize: 11, color: '#a5b4fc' }}>{t.city}</td>
                                                    <td>
                                                      {t.side !== 'NONE' && (
                                                        <span style={{
                                                          color: t.side === 'NO' ? '#fb923c' : '#6ee7b7',
                                                          fontWeight: 600, fontSize: 11,
                                                        }}>{t.side}</span>
                                                      )}
                                                    </td>
                                                    <td><ActionBadge action={t.action} /></td>
                                                    <td style={{ fontSize: 10, color: '#a5b4fc', fontFamily: 'monospace', whiteSpace: 'nowrap' }}>
                                                      {parseMarketDesc(t.market_slug ?? '')}
                                                    </td>
                                                    <td style={{ fontSize: 10, color: '#94a3b8' }}>{t.model}</td>
                                                    <td>{t.p_yes_at_entry != null ? t.p_yes_at_entry.toFixed(3) : '—'}</td>
                                                    <td>{t.lead_days != null ? `${t.lead_days}d` : '—'}</td>
                                                    <td>{t.hold_sec != null ? `${t.hold_sec}s` : '—'}</td>
                                                    <td className={t.realized_pnl_usdc != null
                                                      ? (t.realized_pnl_usdc >= 0 ? 'green' : 'red') : ''}>
                                                      {t.realized_pnl_usdc != null ? usd(t.realized_pnl_usdc) : '—'}
                                                    </td>
                                                  </tr>
                                                )
                                              })}
                                            </tbody>
                                          </table>
                                        </div>

                                        {filteredTrades.length > pageSize && (
                                          <Paginator
                                            total={filteredTrades.length}
                                            page={page}
                                            pageSize={pageSize}
                                            onPage={p => setPageMap(prev => ({ ...prev, [s.strategy_id]: p }))}
                                            onPageSize={n => {
                                              setPageSizeMap(prev => ({ ...prev, [s.strategy_id]: n }))
                                              setPageMap(prev => ({ ...prev, [s.strategy_id]: 0 }))
                                            }}
                                          />
                                        )}
                                      </>
                                    )}
                                  </>
                                )}

                                {/* Forecast shift note */}
                                {weatherStats?.forecast_shift_analysis.count != null &&
                                  weatherStats.forecast_shift_analysis.count > 0 && (
                                  <div style={{
                                    marginTop: 10, paddingTop: 8,
                                    borderTop: '1px solid #141428',
                                    color: '#64748b', fontSize: 11,
                                  }}>
                                    <strong style={{ color: '#a78bfa' }}>Forecast Shift</strong>
                                    {weatherStats.forecast_shift_analysis.avg_p_yes_delta != null && (
                                      <> &nbsp;·&nbsp; avg |Δp_yes|: {weatherStats.forecast_shift_analysis.avg_p_yes_delta.toFixed(3)}</>
                                    )}
                                    {weatherStats.forecast_shift_analysis.pct_direction_flipped != null && (
                                      <> &nbsp;·&nbsp; flipped: {pct(weatherStats.forecast_shift_analysis.pct_direction_flipped)}</>
                                    )}
                                  </div>
                                )}

                              </div>
                            </td>
                          </tr>
                        )}
                      </Fragment>
                    )
                  })}
                </tbody>
              </table>
            </div>
          )}
      </section>

      {/* Exit trigger distribution */}
      {exitTotal > 0 && (
        <section className="panel">
          <h2>Exit Trigger Distribution (7d)</h2>
          <div className="rejection-grid">
            {([
              ['Take Profit',     exits!.take_profit,     '#34d399'],
              ['Stop Loss',       exits!.stop_loss,       '#f87171'],
              ['Forecast Shift',  exits!.forecast_shift,  '#a78bfa'],
              ['Time Decay Exit', exits!.time_decay_exit, '#fb923c'],
            ] as [string, number, string][]).map(([label, count, color]) => (
              <div key={label} className="rejection-card">
                <div className="rejection-label">{label}</div>
                <div className="rejection-value" style={{ color }}>{count}</div>
                <div className="rejection-bar">
                  <div className="rejection-fill"
                    style={{ width: `${Math.min(100, (count / exitTotal) * 100)}%`, background: color }} />
                </div>
              </div>
            ))}
          </div>
        </section>
      )}

      {/* ── Weather Ladder section ─────────────────────────────────────── */}
      <section className="panel" style={{ marginTop: 8 }}>
        <h2 style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
          Weather Ladder
          <span style={{
            fontSize: 10, fontWeight: 700, letterSpacing: '0.06em',
            background: 'rgba(96,165,250,0.12)', border: '1px solid rgba(96,165,250,0.30)',
            borderRadius: 8, padding: '2px 8px', color: '#60a5fa',
          }}>Phase 5b — 高賠率梯形</span>
        </h2>
        {ladderStrategies.length === 0
          ? <p className="empty">No weather_ladder strategy data yet.</p>
          : (
            <div className="table-wrap">
              <table>
                <thead><tr>
                  <th>Strategy</th><th>Ladders</th><th>Open</th>
                  <th>Avg Payout</th><th>Invested</th><th>Net PnL</th>
                  <th>PnL % (alloc)</th><th>Cities</th>
                </tr></thead>
                <tbody>
                  {ladderStrategies.map(s => {
                    const isExpanded = ladderExpandedId === s.strategy_id
                    const cached = ladderDetailCache[s.strategy_id]
                    type LF = 'all' | 'entry' | 'exit'
                    const allTrades   = cached?.recent_trades ?? []
                    const entryTrades = allTrades.filter(t => t.action === 'LADDER_ENTRY')
                    const exitTrades  = allTrades.filter(t => t.action !== 'LADDER_ENTRY')
                    const activeFilter = (ladderFilterMap[s.strategy_id] ?? 'all') as LF
                    const filteredTrades =
                      activeFilter === 'entry' ? entryTrades :
                      activeFilter === 'exit'  ? exitTrades  : allTrades
                    const page     = pageMap[`l_${s.strategy_id}`] ?? 0
                    const pageSize = pageSizeMap[`l_${s.strategy_id}`] ?? 10
                    const shownTrades = filteredTrades.slice(page * pageSize, (page + 1) * pageSize)

                    return (
                      <Fragment key={s.strategy_id}>
                        <tr onClick={() => setLadderExpandedId(prev => prev === s.strategy_id ? null : s.strategy_id)}
                          style={{ cursor: 'pointer' }}
                          className={isExpanded ? 'row-selected' : ''}>
                          <td className="slug">
                            <span style={{ marginRight: 6, opacity: 0.5, fontSize: 10, userSelect: 'none' }}>
                              {isExpanded ? '▼' : '▶'}
                            </span>
                            {s.strategy_id}
                            {s.open_ladders > 0 && (
                              <span style={{
                                marginLeft: 8, display: 'inline-flex', alignItems: 'center', gap: 4,
                                background: 'rgba(96,165,250,0.12)',
                                border: '1px solid rgba(96,165,250,0.35)',
                                borderRadius: 10, padding: '1px 7px',
                                fontSize: 9, fontWeight: 700, color: '#60a5fa',
                              }}>
                                <span style={{ width: 5, height: 5, borderRadius: '50%',
                                  background: '#60a5fa', boxShadow: '0 0 4px #60a5fa', display: 'inline-block' }} />
                                {s.open_ladders}
                              </span>
                            )}
                          </td>
                          <td>{s.total_ladders}</td>
                          <td style={{ color: s.open_ladders > 0 ? '#60a5fa' : '#64748b' }}>{s.open_ladders}</td>
                          <td style={{ color: '#fbbf24' }}>{s.avg_payout_ratio > 0 ? `${s.avg_payout_ratio.toFixed(0)}x` : '—'}</td>
                          <td>{s.total_invested_usdc.toFixed(2)}</td>
                          <td className={s.net_pnl_usdc >= 0 ? 'green' : 'red'}>{usd(s.net_pnl_usdc)}</td>
                          <td className={s.pnl_pct_alloc >= 0 ? 'green' : 'red'}>{signedPct(s.pnl_pct_alloc)}</td>
                          <td>{s.total_cities}</td>
                        </tr>

                        {isExpanded && (
                          <tr>
                            <td colSpan={8} style={{ padding: 0, background: '#080812', borderBottom: '1px solid #1a1a2e' }}>
                              <div style={{ padding: '14px 18px 16px' }}>

                                {/* Open ladders */}
                                {(cached?.open_ladders_list ?? []).length > 0 && (
                                  <div style={{
                                    background: 'rgba(96,165,250,0.04)',
                                    border: '1px solid rgba(96,165,250,0.22)',
                                    borderRadius: 6, marginBottom: 12, overflow: 'hidden',
                                  }}>
                                    <div style={{
                                      display: 'flex', alignItems: 'center', gap: 8, padding: '6px 12px',
                                      borderBottom: '1px solid rgba(96,165,250,0.15)',
                                      background: 'rgba(96,165,250,0.06)',
                                    }}>
                                      <span style={{ width: 7, height: 7, borderRadius: '50%', flexShrink: 0,
                                        background: '#60a5fa', boxShadow: '0 0 5px #60a5fa', display: 'inline-block' }} />
                                      <span style={{ fontSize: 11, fontWeight: 700, color: '#60a5fa', letterSpacing: '0.06em' }}>
                                        OPEN {cached!.open_ladders_list.length}
                                      </span>
                                      <span style={{ fontSize: 10, color: '#64748b', marginLeft: 4 }}>— 未到期梯形</span>
                                    </div>
                                    <div style={{ display: 'grid',
                                      gridTemplateColumns: 'repeat(8, min-content)',
                                      alignItems: 'center', gap: '0 14px', padding: '0 12px',
                                    }}>
                                      {['CITY','DATE','LEGS','SUM','PAYOUT','COMB_P','LEAD','ENTRY'].map(h => (
                                        <span key={h} style={{ fontSize: 9, color: '#4b5563', fontWeight: 700,
                                          letterSpacing: '0.08em', padding: '5px 0 3px' }}>{h}</span>
                                      ))}
                                      {cached!.open_ladders_list.map(l => (
                                        <Fragment key={l.ladder_id}>
                                          <span style={{ fontFamily: 'monospace', fontSize: 11, fontWeight: 600, color: '#60a5fa', paddingBottom: 5 }}>{l.city}</span>
                                          <span style={{ fontSize: 10, color: '#a5b4fc', fontFamily: 'monospace', paddingBottom: 5 }}>{l.target_date}</span>
                                          <span style={{ fontSize: 11, color: '#e2e8f0', paddingBottom: 5 }}>{l.legs}</span>
                                          <span style={{ fontSize: 11, color: '#94a3b8', paddingBottom: 5 }}>{l.sum_price.toFixed(4)}</span>
                                          <span style={{ fontSize: 11, fontWeight: 700, color: '#fbbf24', paddingBottom: 5 }}>{l.payout_ratio.toFixed(0)}x</span>
                                          <span style={{ fontSize: 11, color: '#94a3b8', paddingBottom: 5 }}>{l.combined_p.toFixed(3)}</span>
                                          <span style={{ fontSize: 11, color: '#8892a4', paddingBottom: 5 }}>{l.lead_days != null ? `${l.lead_days}d` : '—'}</span>
                                          <span style={{ fontSize: 10, color: '#64748b', fontFamily: 'monospace', paddingBottom: 5 }}>{l.entry_ts?.slice(11, 19)}</span>
                                        </Fragment>
                                      ))}
                                    </div>
                                  </div>
                                )}

                                {/* Stats strip */}
                                {cached && (
                                  <div style={{ display: 'flex', gap: 12, fontSize: 11, color: '#8892a4', flexWrap: 'wrap', alignItems: 'center', marginBottom: 10 }}>
                                    <span><span style={{ color: '#64748b' }}>Alloc </span>
                                      <strong style={{ color: '#e2e8f0' }}>{cached.initial_allocated_usdc.toFixed(0)} USDC</strong></span>
                                    <span>HTR <strong style={{ color: '#34d399' }}>{cached.htr_count}</strong>
                                      {' '}· CAT <strong style={{ color: '#f87171' }}>{cached.cat_count}</strong></span>
                                    <span><span style={{ color: '#64748b' }}>avg payout </span>
                                      <strong style={{ color: '#fbbf24' }}>{cached.avg_payout_ratio.toFixed(0)}x</strong></span>
                                  </div>
                                )}

                                {/* Filter + trade log */}
                                {allTrades.length > 0 && (
                                  <>
                                    <div style={{ display: 'flex', gap: 4, marginBottom: 8, borderTop: '1px solid #141428', paddingTop: 10 }}>
                                      {([
                                        ['all',   'All',    allTrades.length,   '#94a3b8'],
                                        ['entry', 'Open',   entryTrades.length, '#60a5fa'],
                                        ['exit',  'Closed', exitTrades.length,  '#a78bfa'],
                                      ] as [string, string, number, string][]).map(([f, label, count, color]) => (
                                        <button key={f}
                                          onClick={e => {
                                            e.stopPropagation()
                                            setLadderFilterMap(prev => ({ ...prev, [s.strategy_id]: f }))
                                            setPageMap(prev => ({ ...prev, [`l_${s.strategy_id}`]: 0 }))
                                          }}
                                          style={{
                                            background: activeFilter === f ? '#1e1e42' : 'transparent',
                                            border: `1px solid ${activeFilter === f ? '#818cf8' : '#2e3348'}`,
                                            borderRadius: 4, padding: '3px 10px', cursor: 'pointer',
                                            fontSize: 11, fontWeight: 600,
                                            color: activeFilter === f ? '#e2e8f0' : '#8892a4',
                                            display: 'inline-flex', alignItems: 'center', gap: 5,
                                          }}
                                        >
                                          {label}
                                          {count > 0 && <span style={{ background: '#0e0e1a', borderRadius: 8, padding: '0 5px', fontSize: 10, color }}>{count}</span>}
                                        </button>
                                      ))}
                                    </div>
                                    {filteredTrades.length === 0
                                      ? <p style={{ color: '#8892a4', fontSize: 11, padding: '4px 0' }}>— 無紀錄</p>
                                      : (
                                        <>
                                          <div className="table-wrap">
                                            <table>
                                              <thead><tr>
                                                <th>Time</th><th>City</th><th>Date</th>
                                                <th>Action</th><th>Leg</th><th>Price</th>
                                                <th>Payout</th><th>Comb_P</th><th>Lead</th><th>PnL</th>
                                              </tr></thead>
                                              <tbody>
                                                {shownTrades.map(t => (
                                                  <tr key={t.id}
                                                    className={t.realized_pnl_usdc != null ? (t.realized_pnl_usdc >= 0 ? 'row-win' : 'row-loss') : t.action === 'LADDER_ENTRY' ? 'row-win' : ''}>
                                                    <td className="ts">{t.ts?.slice(11, 19) ?? '—'}</td>
                                                    <td style={{ fontFamily: 'monospace', fontSize: 11, color: '#60a5fa' }}>{t.city}</td>
                                                    <td style={{ fontSize: 10, color: '#a5b4fc', fontFamily: 'monospace' }}>{t.target_date}</td>
                                                    <td><LadderActionBadge action={t.action} /></td>
                                                    <td style={{ color: '#64748b', fontSize: 11 }}>{t.leg_index}</td>
                                                    <td>{t.price.toFixed(4)}</td>
                                                    <td style={{ color: '#fbbf24', fontWeight: 700 }}>{t.payout_ratio > 0 ? `${t.payout_ratio.toFixed(0)}x` : '—'}</td>
                                                    <td style={{ color: '#94a3b8' }}>{t.combined_p.toFixed(3)}</td>
                                                    <td>{t.lead_days != null ? `${t.lead_days}d` : '—'}</td>
                                                    <td className={t.realized_pnl_usdc != null ? (t.realized_pnl_usdc >= 0 ? 'green' : 'red') : ''}>
                                                      {t.realized_pnl_usdc != null ? usd(t.realized_pnl_usdc) : '—'}
                                                    </td>
                                                  </tr>
                                                ))}
                                              </tbody>
                                            </table>
                                          </div>
                                          {filteredTrades.length > pageSize && (
                                            <Paginator
                                              total={filteredTrades.length}
                                              page={page} pageSize={pageSize}
                                              onPage={p => setPageMap(prev => ({ ...prev, [`l_${s.strategy_id}`]: p }))}
                                              onPageSize={n => {
                                                setPageSizeMap(prev => ({ ...prev, [`l_${s.strategy_id}`]: n }))
                                                setPageMap(prev => ({ ...prev, [`l_${s.strategy_id}`]: 0 }))
                                              }}
                                            />
                                          )}
                                        </>
                                      )}
                                  </>
                                )}
                              </div>
                            </td>
                          </tr>
                        )}
                      </Fragment>
                    )
                  })}
                </tbody>
              </table>
            </div>
          )}
      </section>
    </>
  )
}

// ── Root App ──────────────────────────────────────────────────────────────────

type Tab = 'btc' | 'mention' | 'weather'

export default function App() {
  const [tab, setTab] = useState<Tab>(() =>
    (sessionStorage.getItem('activeTab') as Tab | null) ?? 'btc'
  )
  const [wsStatus, setWsStatus] = useState<'connecting' | 'live' | 'offline'>('connecting')

  useEffect(() => {
    sessionStorage.setItem('activeTab', tab)
  }, [tab])

  // WebSocket for live badge updates (data handled inside each tab via REST)
  useEffect(() => {
    const proto = window.location.protocol === 'https:' ? 'wss' : 'ws'
    const ws = new WebSocket(`${proto}://${window.location.host}/ws/live`)
    ws.onopen  = () => setWsStatus('live')
    ws.onclose = () => setWsStatus('offline')
    ws.onerror = () => setWsStatus('offline')
    return () => ws.close()
  }, [])

  return (
    <div className="app">
      {/* Header */}
      <header className="app-header">
        <h1>
          Polymarket Arb&nbsp;
          <span className="mode-badge">DRY_RUN</span>
        </h1>
        <div className={`ws-dot ws-${wsStatus}`} title={`WebSocket: ${wsStatus}`} />
      </header>

      {/* Tab selector */}
      <nav className="tab-bar">
        <button
          className={`tab-btn${tab === 'btc' ? ' tab-active' : ''}`}
          onClick={() => setTab('btc')}
        >
          BTC 15m
          <span className="tab-sub">Dump-Hedge / Pure-Arb</span>
        </button>
        <button
          className={`tab-btn${tab === 'mention' ? ' tab-active' : ''}`}
          onClick={() => setTab('mention')}
        >
          Mention Market
          <span className="tab-sub">Phase 4 — Trump</span>
        </button>
        <button
          className={`tab-btn${tab === 'weather' ? ' tab-active' : ''}`}
          onClick={() => setTab('weather')}
        >
          Weather Market
          <span className="tab-sub">Phase 5 — GFS/ECMWF</span>
        </button>
      </nav>

      {tab === 'btc'     && <BtcTab />}
      {tab === 'mention' && <MentionTab />}
      {tab === 'weather' && <WeatherTab />}
    </div>
  )
}
