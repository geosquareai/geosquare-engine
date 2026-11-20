//! Raw C API plumbing for functions that run SQL inside DuckDB.
//!
//! Dataset functions plan an inner query (read_parquet / iceberg_scan / COPY) and run
//! it on connections owned by the extension. This module avoids the duckdb-rs result
//! path on purpose: it converts results through Arrow, which crashes when an extension
//! calls it on DuckDB 1.4/1.5.0. Everything here uses the stable C API only.
//!
//! Connections are opened while the database handle from the loader is valid (during
//! `LOAD`) and kept in a small pool; a handle saved for later use is not valid. Each
//! database that loads the extension gets its own pool, carried by the functions
//! registered in that database and made current on the thread while they run, so a
//! process with several databases runs inner queries in the right one.

use duckdb::ffi::*;
use std::{
    cell::RefCell,
    collections::HashMap,
    error::Error,
    ffi::{c_void, CStr, CString},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

pub type E = Box<dyn Error>;

/// Text rows returned by a query or an action (NULL as None).
pub type TextRows = Vec<Vec<Option<String>>>;
/// A side effect run once at execution.
pub type ActionFn = Box<dyn FnOnce() -> Result<TextRows, E> + Send>;

pub struct Pool {
    conns: Vec<(duckdb_connection, AtomicBool)>,
}
unsafe impl Send for Pool {}
unsafe impl Sync for Pool {}

thread_local! {
    /// The pool of the database whose function is running on this thread.
    static CURRENT: RefCell<Option<Arc<Pool>>> = const { RefCell::new(None) };
}

/// Open `n` connections for inner queries. Call during `LOAD`, once per database.
pub unsafe fn init_pool(db: duckdb_database, n: usize) -> Result<Arc<Pool>, E> {
    let mut conns = Vec::with_capacity(n);
    for _ in 0..n {
        let mut c: duckdb_connection = std::ptr::null_mut();
        if unsafe { duckdb_connect(db, &mut c) } != duckdb_state_DuckDBSuccess {
            return Err("could not open an internal connection".into());
        }
        conns.push((c, AtomicBool::new(false)));
    }
    Ok(Arc::new(Pool { conns }))
}

/// The pool of the database whose function is running on this thread.
pub fn current_pool() -> Option<Arc<Pool>> {
    CURRENT.with(|c| c.borrow().clone())
}

/// Run `f(lease, item)` for every item on up to `threads` threads, each with its own
/// connection of this thread's pool. Results keep the input order; the first error stops
/// the remaining items.
pub fn par_map<T: Send, R: Send>(
    items: Vec<T>,
    threads: usize,
    f: impl Fn(&Lease, T) -> Result<R, E> + Sync,
) -> Result<Vec<R>, E> {
    use std::sync::atomic::AtomicBool;
    use std::sync::Mutex;
    let pool = current_pool().ok_or("geosquare_v2: no internal connections for this database")?;
    let n = items.len();
    let threads = threads.clamp(1, n.max(1));
    // No threads in DuckDB-Wasm: run in order on this thread (and link no thread code).
    #[cfg(target_family = "wasm")]
    {
        let _ = (pool, n, threads);
        let l = lease()?;
        return items.into_iter().map(|item| f(&l, item)).collect();
    }
    #[cfg(not(target_family = "wasm"))]
    if threads == 1 {
        let l = lease()?;
        return items.into_iter().map(|item| f(&l, item)).collect();
    }
    let queue = Mutex::new(items.into_iter().enumerate());
    let results: Mutex<Vec<Option<R>>> = Mutex::new((0..n).map(|_| None).collect());
    let failed: Mutex<Option<String>> = Mutex::new(None);
    let stop = AtomicBool::new(false);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                with_pool(&pool, || {
                    let lease = match lease() {
                        Ok(l) => l,
                        Err(e) => {
                            stop.store(true, Ordering::Release);
                            failed.lock().unwrap().get_or_insert(e.to_string());
                            return;
                        }
                    };
                    while !stop.load(Ordering::Acquire) {
                        let Some((i, item)) = queue.lock().unwrap().next() else {
                            break;
                        };
                        match f(&lease, item) {
                            Ok(r) => results.lock().unwrap()[i] = Some(r),
                            Err(e) => {
                                stop.store(true, Ordering::Release);
                                failed.lock().unwrap().get_or_insert(e.to_string());
                            }
                        }
                    }
                })
            });
        }
    });
    if let Some(e) = failed.into_inner().unwrap() {
        return Err(e.into());
    }
    Ok(results
        .into_inner()
        .unwrap()
        .into_iter()
        .map(|r| r.expect("every item ran"))
        .collect())
}

/// Run `f` with `pool` as this thread's current pool (restoring the previous one).
pub fn with_pool<R>(pool: &Arc<Pool>, f: impl FnOnce() -> R) -> R {
    let prev = CURRENT.with(|c| c.replace(Some(pool.clone())));
    let out = f();
    CURRENT.with(|c| *c.borrow_mut() = prev);
    out
}

/// One borrowed pool connection, released on drop.
pub struct Lease {
    pool: Arc<Pool>,
    idx: usize,
}

impl Lease {
    pub fn conn(&self) -> duckdb_connection {
        self.pool.conns[self.idx].0
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.pool.conns[self.idx].1.store(false, Ordering::Release);
    }
}

pub fn lease() -> Result<Lease, E> {
    let pool = CURRENT.with(|c| c.borrow().clone()).ok_or(
        "geosquare_v2: no internal connections for this database (was the extension loaded?)",
    )?;
    for (i, (_, busy)) in pool.conns.iter().enumerate() {
        if busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return Ok(Lease {
                pool: pool.clone(),
                idx: i,
            });
        }
    }
    Err(format!(
        "geosquare_v2: all {} internal connections are busy; run fewer dataset scans at once \
         or set GEOSQUARE_V2_CONNECTIONS before LOAD",
        pool.conns.len()
    )
    .into())
}

unsafe fn take_cstr(p: *const std::os::raw::c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

/// Run `sql` and return every value as text (NULL as None). For small metadata queries.
pub fn query_text_on(lease: &Lease, sql: &str) -> Result<Vec<Vec<Option<String>>>, E> {
    let csql = CString::new(sql)?;
    unsafe {
        let mut res: duckdb_result = std::mem::zeroed();
        if duckdb_query(lease.conn(), csql.as_ptr(), &mut res) != duckdb_state_DuckDBSuccess {
            let e = take_cstr(duckdb_result_error(&mut res));
            duckdb_destroy_result(&mut res);
            return Err(e.into());
        }
        let rows = duckdb_row_count(&mut res);
        let cols = duckdb_column_count(&mut res);
        let mut out = Vec::with_capacity(rows as usize);
        for r in 0..rows {
            let mut row = Vec::with_capacity(cols as usize);
            for c in 0..cols {
                if duckdb_value_is_null(&mut res, c, r) {
                    row.push(None);
                } else {
                    let p = duckdb_value_varchar(&mut res, c, r);
                    row.push(Some(take_cstr(p)));
                    duckdb_free(p as *mut c_void);
                }
            }
            out.push(row);
        }
        duckdb_destroy_result(&mut res);
        Ok(out)
    }
}

pub fn exec_on(lease: &Lease, sql: &str) -> Result<(), E> {
    query_text_on(lease, sql).map(|_| ())
}

/// SQL string literal.
pub fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// SQL identifier.
pub fn ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

// ---------------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum Param {
    Null,
    Text(String),
    Int(i64),
    Float(f64),
    List(Vec<Param>),
}

impl Param {
    pub fn text(&self) -> Option<&str> {
        match self {
            Param::Text(s) => Some(s),
            _ => None,
        }
    }
    pub fn int(&self) -> Option<i64> {
        match self {
            Param::Int(v) => Some(*v),
            Param::Float(v) => Some(*v as i64),
            _ => None,
        }
    }
    pub fn float(&self) -> Option<f64> {
        match self {
            Param::Float(v) => Some(*v),
            Param::Int(v) => Some(*v as f64),
            _ => None,
        }
    }
}

/// Declared parameter kinds.
#[derive(Clone, Copy)]
pub enum Kind {
    Text,
    Int,
    Bool,
    Float,
    FloatList,
    TextList,
    /// ANY: a VARCHAR or a list of VARCHAR (read as Param::Text or Param::List).
    TextOrList,
}

impl Kind {
    unsafe fn logical(self) -> duckdb_logical_type {
        unsafe {
            match self {
                Kind::Text => duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_VARCHAR),
                Kind::Int => duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_BIGINT),
                Kind::Bool => duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_BOOLEAN),
                Kind::Float => duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_DOUBLE),
                Kind::FloatList => {
                    let mut child = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_DOUBLE);
                    let l = duckdb_create_list_type(child);
                    duckdb_destroy_logical_type(&mut child);
                    l
                }
                Kind::TextList => {
                    let mut child = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_VARCHAR);
                    let l = duckdb_create_list_type(child);
                    duckdb_destroy_logical_type(&mut child);
                    l
                }
                Kind::TextOrList => duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_ANY),
            }
        }
    }

    unsafe fn read(self, mut v: duckdb_value) -> Param {
        if v.is_null() {
            return Param::Null;
        }
        let p = unsafe {
            if duckdb_is_null_value(v) {
                Param::Null
            } else {
                match self {
                    Kind::Text => {
                        let s = duckdb_get_varchar(v);
                        let t = take_cstr(s);
                        duckdb_free(s as *mut c_void);
                        Param::Text(t)
                    }
                    Kind::Int => Param::Int(duckdb_get_int64(v)),
                    Kind::Bool => Param::Int(duckdb_get_bool(v) as i64),
                    Kind::Float => Param::Float(duckdb_get_double(v)),
                    Kind::FloatList => {
                        let n = duckdb_get_list_size(v);
                        let mut items = Vec::with_capacity(n as usize);
                        for i in 0..n {
                            let c = duckdb_get_list_child(v, i);
                            items.push(Kind::Float.read(c));
                        }
                        Param::List(items)
                    }
                    Kind::TextOrList => {
                        let mut t = duckdb_get_value_type(v);
                        let id = duckdb_get_type_id(t);
                        let _ = &mut t; // borrowed from the value: not destroyed here
                        if id == DUCKDB_TYPE_DUCKDB_TYPE_LIST {
                            let n = duckdb_get_list_size(v);
                            let mut items = Vec::with_capacity(n as usize);
                            for i in 0..n {
                                let c = duckdb_get_list_child(v, i);
                                items.push(Kind::Text.read(c));
                            }
                            Param::List(items)
                        } else {
                            let s = duckdb_get_varchar(v);
                            let t = take_cstr(s);
                            duckdb_free(s as *mut c_void);
                            Param::Text(t)
                        }
                    }
                    Kind::TextList => {
                        let n = duckdb_get_list_size(v);
                        let mut items = Vec::with_capacity(n as usize);
                        for i in 0..n {
                            let c = duckdb_get_list_child(v, i);
                            items.push(Kind::Text.read(c));
                        }
                        Param::List(items)
                    }
                }
            }
        };
        unsafe { duckdb_destroy_value(&mut v) };
        p
    }
}

pub struct Params {
    pub positional: Vec<Param>,
    pub named: HashMap<String, Param>,
}

impl Params {
    pub fn pos_text(&self, i: usize, name: &str) -> Result<String, E> {
        self.positional
            .get(i)
            .and_then(|p| p.text())
            .map(str::to_string)
            .ok_or_else(|| format!("{name} must not be NULL").into())
    }
    pub fn get(&self, name: &str) -> Option<&Param> {
        self.named.get(name).filter(|p| !matches!(p, Param::Null))
    }
    pub fn text(&self, name: &str) -> Option<String> {
        self.get(name).and_then(|p| p.text()).map(str::to_string)
    }
    pub fn int(&self, name: &str) -> Option<i64> {
        self.get(name).and_then(Param::int)
    }
    pub fn float(&self, name: &str) -> Option<f64> {
        self.get(name).and_then(Param::float)
    }
}

// ---------------------------------------------------------------------------
// Proxy table functions: plan SQL in bind, stream its result
// ---------------------------------------------------------------------------

/// What a proxy table function does at bind time.
pub enum Plan {
    /// Stream the rows of this query.
    Query(String),
    /// Stream the rows of the first query; its columns are those of the second, a query
    /// that reads nothing (`SELECT NULL::type AS name, ...`), so binding does no I/O.
    Typed(String, String),
    /// Run a side-effecting action once at execution, then return its rows.
    Action(ActionFn, Vec<String>),
}

pub type Planner = fn(&Params) -> Result<Plan, E>;

struct Spec {
    pool: Arc<Pool>,
    planner: Planner,
    positional: Vec<Kind>,
    named: Vec<(String, Kind)>,
}

struct Bound {
    pool: Arc<Pool>,
    kind: BoundKind,
}

enum BoundKind {
    /// Inner query and its column names (in bind order).
    Query(String, Vec<String>),
    Action(std::sync::Mutex<Option<ActionFn>>, usize),
}

struct Running {
    _lease: Option<Lease>,
    prepared: duckdb_prepared_statement,
    result: Option<duckdb_result>,
    action_rows: Option<Vec<Vec<Option<String>>>>,
    action_done: bool,
}

/// Physical width in bytes for fixed-width types, None for strings, Err for others.
unsafe fn width(t: duckdb_logical_type) -> Result<Option<usize>, ()> {
    unsafe {
        let id = duckdb_get_type_id(t);
        Ok(Some(match id {
            DUCKDB_TYPE_DUCKDB_TYPE_BOOLEAN
            | DUCKDB_TYPE_DUCKDB_TYPE_TINYINT
            | DUCKDB_TYPE_DUCKDB_TYPE_UTINYINT => 1,
            DUCKDB_TYPE_DUCKDB_TYPE_SMALLINT | DUCKDB_TYPE_DUCKDB_TYPE_USMALLINT => 2,
            DUCKDB_TYPE_DUCKDB_TYPE_INTEGER
            | DUCKDB_TYPE_DUCKDB_TYPE_UINTEGER
            | DUCKDB_TYPE_DUCKDB_TYPE_FLOAT
            | DUCKDB_TYPE_DUCKDB_TYPE_DATE => 4,
            DUCKDB_TYPE_DUCKDB_TYPE_BIGINT
            | DUCKDB_TYPE_DUCKDB_TYPE_UBIGINT
            | DUCKDB_TYPE_DUCKDB_TYPE_DOUBLE
            | DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP
            | DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP_S
            | DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP_MS
            | DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP_NS
            | DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP_TZ
            | DUCKDB_TYPE_DUCKDB_TYPE_TIME
            | DUCKDB_TYPE_DUCKDB_TYPE_TIME_TZ => 8,
            DUCKDB_TYPE_DUCKDB_TYPE_HUGEINT
            | DUCKDB_TYPE_DUCKDB_TYPE_UHUGEINT
            | DUCKDB_TYPE_DUCKDB_TYPE_UUID
            | DUCKDB_TYPE_DUCKDB_TYPE_INTERVAL => 16,
            DUCKDB_TYPE_DUCKDB_TYPE_DECIMAL => match duckdb_decimal_internal_type(t) {
                DUCKDB_TYPE_DUCKDB_TYPE_SMALLINT => 2,
                DUCKDB_TYPE_DUCKDB_TYPE_INTEGER => 4,
                DUCKDB_TYPE_DUCKDB_TYPE_BIGINT => 8,
                _ => 16,
            },
            DUCKDB_TYPE_DUCKDB_TYPE_VARCHAR | DUCKDB_TYPE_DUCKDB_TYPE_BLOB => return Ok(None),
            _ => return Err(()),
        }))
    }
}

/// Whether the proxy can copy a column of type `t`: fixed-width values, text/blob, and
/// lists of those (nested).
unsafe fn supported(t: duckdb_logical_type) -> bool {
    unsafe {
        if duckdb_get_type_id(t) == DUCKDB_TYPE_DUCKDB_TYPE_LIST {
            let mut child = duckdb_list_type_child_type(t);
            let ok = supported(child);
            duckdb_destroy_logical_type(&mut child);
            return ok;
        }
        width(t).is_ok()
    }
}

/// Column names and logical types of `sql` (caller destroys the types).
unsafe fn schema(lease: &Lease, sql: &str) -> Result<Vec<(String, duckdb_logical_type)>, E> {
    let q = CString::new(format!("SELECT * FROM ({sql}) AS gs_inner LIMIT 0"))?;
    unsafe {
        let mut res: duckdb_result = std::mem::zeroed();
        if duckdb_query(lease.conn(), q.as_ptr(), &mut res) != duckdb_state_DuckDBSuccess {
            let e = take_cstr(duckdb_result_error(&mut res));
            duckdb_destroy_result(&mut res);
            return Err(e.into());
        }
        let n = duckdb_column_count(&mut res);
        let mut cols = Vec::with_capacity(n as usize);
        for c in 0..n {
            cols.push((
                take_cstr(duckdb_column_name(&mut res, c)),
                duckdb_column_logical_type(&mut res, c),
            ));
        }
        duckdb_destroy_result(&mut res);
        Ok(cols)
    }
}

/// Wrap columns whose type the copier does not handle in `CAST(... AS VARCHAR)`.
unsafe fn finalize(
    lease: &Lease,
    sql: String,
) -> Result<(String, Vec<(String, duckdb_logical_type)>), E> {
    unsafe {
        let mut cols = schema(lease, &sql)?;
        if cols.iter().all(|(_, t)| supported(*t)) {
            return Ok((sql, cols));
        }
        let select: Vec<String> = cols
            .iter()
            .map(|(n, t)| {
                if supported(*t) {
                    ident(n)
                } else {
                    format!("CAST({} AS VARCHAR) AS {}", ident(n), ident(n))
                }
            })
            .collect();
        for (_, t) in cols.iter_mut() {
            duckdb_destroy_logical_type(t);
        }
        let wrapped = format!("SELECT {} FROM ({sql}) AS gs_cast", select.join(", "));
        let cols = schema(lease, &wrapped)?;
        Ok((wrapped, cols))
    }
}

unsafe extern "C" fn destroy_box<T>(p: *mut c_void) {
    if !p.is_null() {
        drop(unsafe { Box::from_raw(p as *mut T) });
    }
}

unsafe fn set_bind_error(info: duckdb_bind_info, e: &str) {
    let c = CString::new(e.replace('\0', " ")).unwrap_or_default();
    unsafe { duckdb_bind_set_error(info, c.as_ptr()) };
}

unsafe extern "C" fn proxy_bind(info: duckdb_bind_info) {
    unsafe {
        let spec = &*(duckdb_bind_get_extra_info(info) as *const Spec);
        let mut positional = Vec::with_capacity(spec.positional.len());
        for (i, k) in spec.positional.iter().enumerate() {
            positional.push(k.read(duckdb_bind_get_parameter(info, i as u64)));
        }
        let mut named = HashMap::new();
        for (name, k) in &spec.named {
            let c = CString::new(name.as_str()).unwrap_or_default();
            named.insert(
                name.clone(),
                k.read(duckdb_bind_get_named_parameter(info, c.as_ptr())),
            );
        }
        let params = Params { positional, named };
        let result: Result<BoundKind, E> =
            with_pool(&spec.pool, || match (spec.planner)(&params)? {
                plan @ (Plan::Query(_) | Plan::Typed(..)) => {
                    let lease = lease()?;
                    let (sql, cols) = match plan {
                        Plan::Typed(sql, probe) => {
                            let cols = schema(&lease, &probe)?;
                            if cols.iter().all(|(_, t)| supported(*t)) {
                                (sql, cols)
                            } else {
                                for (_, mut t) in cols {
                                    duckdb_destroy_logical_type(&mut t);
                                }
                                finalize(&lease, sql)?
                            }
                        }
                        Plan::Query(sql) => finalize(&lease, sql)?,
                        Plan::Action(..) => unreachable!(),
                    };
                    let mut names = Vec::with_capacity(cols.len());
                    for (name, mut t) in cols {
                        let c = CString::new(name.as_str()).unwrap_or_default();
                        duckdb_bind_add_result_column(info, c.as_ptr(), t);
                        duckdb_destroy_logical_type(&mut t);
                        names.push(name);
                    }
                    Ok(BoundKind::Query(sql, names))
                }
                Plan::Action(f, columns) => {
                    for name in &columns {
                        let c = CString::new(name.as_str()).unwrap_or_default();
                        let mut t = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_VARCHAR);
                        duckdb_bind_add_result_column(info, c.as_ptr(), t);
                        duckdb_destroy_logical_type(&mut t);
                    }
                    Ok(BoundKind::Action(
                        std::sync::Mutex::new(Some(f)),
                        columns.len(),
                    ))
                }
            });
        match result.map(|kind| Bound {
            pool: spec.pool.clone(),
            kind,
        }) {
            Ok(b) => duckdb_bind_set_bind_data(
                info,
                Box::into_raw(Box::new(b)) as *mut c_void,
                Some(destroy_box::<Bound>),
            ),
            Err(e) => set_bind_error(info, &e.to_string()),
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        unsafe {
            if let Some(mut r) = self.result.take() {
                duckdb_destroy_result(&mut r);
            }
            if !self.prepared.is_null() {
                duckdb_destroy_prepare(&mut self.prepared);
            }
        }
    }
}

unsafe fn start(sql: &str) -> Result<Running, E> {
    unsafe {
        let lease = lease()?;
        // A streamed result runs ahead of the consumer only by this much; the default (~1 MB)
        // keeps remote scans almost sequential, which is slow on S3/HTTP latency.
        let buffer = std::env::var("GEOSQUARE_V2_STREAM_BUFFER").unwrap_or_else(|_| "256MB".into());
        let _ = exec_on(
            &lease,
            &format!("SET streaming_buffer_size = {}", lit(&buffer)),
        );
        let c = CString::new(sql)?;
        let mut run = Running {
            _lease: None,
            prepared: std::ptr::null_mut(),
            result: None,
            action_rows: None,
            action_done: false,
        };
        if duckdb_prepare(lease.conn(), c.as_ptr(), &mut run.prepared) != duckdb_state_DuckDBSuccess
        {
            let e = take_cstr(duckdb_prepare_error(run.prepared));
            return Err(e.into());
        }
        let mut pending: duckdb_pending_result = std::ptr::null_mut();
        if duckdb_pending_prepared_streaming(run.prepared, &mut pending)
            != duckdb_state_DuckDBSuccess
        {
            let e = take_cstr(duckdb_pending_error(pending));
            duckdb_destroy_pending(&mut pending);
            return Err(e.into());
        }
        let mut res: duckdb_result = std::mem::zeroed();
        let st = duckdb_execute_pending(pending, &mut res);
        duckdb_destroy_pending(&mut pending);
        if st != duckdb_state_DuckDBSuccess {
            let e = take_cstr(duckdb_result_error(&mut res));
            duckdb_destroy_result(&mut res);
            return Err(e.into());
        }
        run.result = Some(res);
        run._lease = Some(lease);
        Ok(run)
    }
}

unsafe extern "C" fn proxy_init(info: duckdb_init_info) {
    unsafe {
        let bound = &*(duckdb_init_get_bind_data(info) as *const Bound);
        let r: Result<Running, E> = match &bound.kind {
            BoundKind::Query(sql, names) => {
                // Projection pushdown: select only the columns the outer query uses, in
                // the order DuckDB asks for them, so the inner scan reads just those.
                let n = duckdb_init_get_column_count(info);
                let cols: Vec<String> = (0..n)
                    .map(|i| {
                        let idx = duckdb_init_get_column_index(info, i) as usize;
                        match names.get(idx) {
                            Some(name) => ident(name),
                            None => "0::BIGINT".to_string(), // row id (e.g. count(*))
                        }
                    })
                    .collect();
                // No column wanted (count(*)): one constant, so the inner scan reads nothing
                // it does not need; only the output's columns are copied.
                let sql = if cols.is_empty() {
                    format!("SELECT 0::BIGINT AS gs_row FROM ({sql}) AS gs_proj")
                } else {
                    format!("SELECT {} FROM ({sql}) AS gs_proj", cols.join(", "))
                };
                with_pool(&bound.pool, || start(&sql))
            }
            BoundKind::Action(..) => Ok(Running {
                _lease: None,
                prepared: std::ptr::null_mut(),
                result: None,
                action_rows: None,
                action_done: false,
            }),
        };
        match r {
            Ok(run) => duckdb_init_set_init_data(
                info,
                Box::into_raw(Box::new(run)) as *mut c_void,
                Some(destroy_box::<Running>),
            ),
            Err(e) => {
                let c = CString::new(e.to_string().replace('\0', " ")).unwrap_or_default();
                duckdb_init_set_error(info, c.as_ptr());
            }
        }
    }
}

/// Copy `n` rows of `src` into `dst`; both vectors have logical type `t`.
unsafe fn copy_vector(
    src: duckdb_vector,
    dst: duckdb_vector,
    n: usize,
    t: duckdb_logical_type,
) -> Result<(), E> {
    unsafe {
        if duckdb_get_type_id(t) == DUCKDB_TYPE_DUCKDB_TYPE_LIST {
            return copy_list(src, dst, n, t);
        }
        let w = width(t).map_err(|_| "unsupported column type in inner query")?;
        let src_valid = duckdb_vector_get_validity(src);
        if !src_valid.is_null() {
            duckdb_vector_ensure_validity_writable(dst);
            let dst_valid = duckdb_vector_get_validity(dst);
            std::ptr::copy_nonoverlapping(src_valid, dst_valid, n.div_ceil(64));
        }
        let is_valid =
            |i: usize| src_valid.is_null() || (*src_valid.add(i / 64) >> (i % 64)) & 1 == 1;
        match w {
            Some(bytes) => {
                std::ptr::copy_nonoverlapping(
                    duckdb_vector_get_data(src) as *const u8,
                    duckdb_vector_get_data(dst) as *mut u8,
                    n * bytes,
                );
            }
            None => {
                let data = duckdb_vector_get_data(src) as *const duckdb_string_t;
                for i in 0..n {
                    if !is_valid(i) {
                        continue;
                    }
                    let s = &*data.add(i);
                    let len = s.value.inlined.length as usize;
                    let ptr = if len <= 12 {
                        s.value.inlined.inlined.as_ptr()
                    } else {
                        s.value.pointer.ptr as *const std::os::raw::c_char
                    };
                    duckdb_vector_assign_string_element_len(dst, i as u64, ptr, len as u64);
                }
            }
        }
        Ok(())
    }
}

/// Copy `n` list rows: validity, the (offset, length) entries, then the child values.
unsafe fn copy_list(
    src: duckdb_vector,
    dst: duckdb_vector,
    n: usize,
    t: duckdb_logical_type,
) -> Result<(), E> {
    unsafe {
        let src_valid = duckdb_vector_get_validity(src);
        if !src_valid.is_null() {
            duckdb_vector_ensure_validity_writable(dst);
            std::ptr::copy_nonoverlapping(
                src_valid,
                duckdb_vector_get_validity(dst),
                n.div_ceil(64),
            );
        }
        let entries = duckdb_vector_get_data(src) as *const duckdb_list_entry;
        let total = duckdb_list_vector_get_size(src);
        if duckdb_list_vector_reserve(dst, total) != duckdb_state_DuckDBSuccess
            || duckdb_list_vector_set_size(dst, total) != duckdb_state_DuckDBSuccess
        {
            return Err("cannot size a list column".into());
        }
        std::ptr::copy_nonoverlapping(
            entries,
            duckdb_vector_get_data(dst) as *mut duckdb_list_entry,
            n,
        );
        let mut child = duckdb_list_type_child_type(t);
        let r = copy_vector(
            duckdb_list_vector_get_child(src),
            duckdb_list_vector_get_child(dst),
            total as usize,
            child,
        );
        duckdb_destroy_logical_type(&mut child);
        r
    }
}

unsafe extern "C" fn proxy_function(info: duckdb_function_info, output: duckdb_data_chunk) {
    unsafe {
        let bound = &*(duckdb_function_get_bind_data(info) as *const Bound);
        let run = &mut *(duckdb_function_get_init_data(info) as *mut Running);
        let r: Result<(), E> = (|| match &bound.kind {
            BoundKind::Action(f, ncols) => {
                if !run.action_done {
                    run.action_done = true;
                    let f = f
                        .lock()
                        .map_err(|_| "poisoned")?
                        .take()
                        .ok_or("action already ran")?;
                    run.action_rows = Some(with_pool(&bound.pool, f)?);
                }
                let rows = run.action_rows.take().unwrap_or_default();
                for c in 0..*ncols {
                    let v = duckdb_data_chunk_get_vector(output, c as u64);
                    for (i, row) in rows.iter().enumerate() {
                        match row.get(c).cloned().flatten() {
                            Some(s) => duckdb_vector_assign_string_element_len(
                                v,
                                i as u64,
                                s.as_ptr() as *const _,
                                s.len() as u64,
                            ),
                            None => {
                                duckdb_vector_ensure_validity_writable(v);
                                duckdb_validity_set_row_invalid(
                                    duckdb_vector_get_validity(v),
                                    i as u64,
                                );
                            }
                        }
                    }
                }
                duckdb_data_chunk_set_size(output, rows.len() as u64);
                Ok(())
            }
            BoundKind::Query(..) => {
                let Some(res) = run.result.as_mut() else {
                    duckdb_data_chunk_set_size(output, 0);
                    return Ok(());
                };
                let mut chunk = duckdb_fetch_chunk(*res);
                if chunk.is_null() {
                    let err = duckdb_result_error(res);
                    if !err.is_null() {
                        return Err(take_cstr(err).into());
                    }
                    duckdb_data_chunk_set_size(output, 0);
                    return Ok(());
                }
                let n = duckdb_data_chunk_get_size(chunk) as usize;
                let cols = duckdb_data_chunk_get_column_count(chunk)
                    .min(duckdb_data_chunk_get_column_count(output));
                for c in 0..cols {
                    let src = duckdb_data_chunk_get_vector(chunk, c);
                    let dst = duckdb_data_chunk_get_vector(output, c);
                    let mut t = duckdb_vector_get_column_type(src);
                    let r = copy_vector(src, dst, n, t);
                    duckdb_destroy_logical_type(&mut t);
                    r?;
                }
                duckdb_data_chunk_set_size(output, n as u64);
                duckdb_destroy_data_chunk(&mut chunk);
                Ok(())
            }
        })();
        if let Err(e) = r {
            let c = CString::new(e.to_string().replace('\0', " ")).unwrap_or_default();
            duckdb_function_set_error(info, c.as_ptr());
        }
    }
}

/// Register a proxy table function on a raw connection.
pub unsafe fn register(
    con: duckdb_connection,
    name: &str,
    positional: Vec<Kind>,
    named: Vec<(&str, Kind)>,
    planner: Planner,
    pool: Arc<Pool>,
) -> Result<(), E> {
    unsafe {
        let mut tf = duckdb_create_table_function();
        let cname = CString::new(name)?;
        duckdb_table_function_set_name(tf, cname.as_ptr());
        for k in &positional {
            let mut t = k.logical();
            duckdb_table_function_add_parameter(tf, t);
            duckdb_destroy_logical_type(&mut t);
        }
        for (n, k) in &named {
            let c = CString::new(*n)?;
            let mut t = k.logical();
            duckdb_table_function_add_named_parameter(tf, c.as_ptr(), t);
            duckdb_destroy_logical_type(&mut t);
        }
        let spec = Spec {
            pool,
            planner,
            positional,
            named: named.into_iter().map(|(n, k)| (n.to_string(), k)).collect(),
        };
        duckdb_table_function_set_extra_info(
            tf,
            Box::into_raw(Box::new(spec)) as *mut c_void,
            Some(destroy_box::<Spec>),
        );
        duckdb_table_function_supports_projection_pushdown(tf, true);
        duckdb_table_function_set_bind(tf, Some(proxy_bind));
        duckdb_table_function_set_init(tf, Some(proxy_init));
        duckdb_table_function_set_function(tf, Some(proxy_function));
        let st = duckdb_register_table_function(con, tf);
        duckdb_destroy_table_function(&mut tf);
        if st != duckdb_state_DuckDBSuccess {
            return Err(format!("could not register table function {name}").into());
        }
        Ok(())
    }
}
