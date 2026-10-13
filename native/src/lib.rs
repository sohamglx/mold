use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Map, Value as JsonValue};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

// ============================================================================
// SqliteConnection
// ============================================================================

/// Native SQLite Connection wrapper for Flame
#[derive(Clone)]
pub struct SqliteConnection {
    conn: Arc<Mutex<Option<Connection>>>,
    last_error: Arc<Mutex<Option<String>>>,
    next_stmt_id: Arc<AtomicI64>,
}

impl SqliteConnection {
    fn new(c: Connection) -> Self {
        Self {
            conn: Arc::new(Mutex::new(Some(c))),
            last_error: Arc::new(Mutex::new(None)),
            next_stmt_id: Arc::new(AtomicI64::new(1)),
        }
    }

    fn new_error(err: String) -> Self {
        Self {
            conn: Arc::new(Mutex::new(None)),
            last_error: Arc::new(Mutex::new(Some(err))),
            next_stmt_id: Arc::new(AtomicI64::new(1)),
        }
    }

    fn set_error(&self, err: String) {
        eprintln!("[mold sqlite error] {}", err);
        if let Ok(mut guard) = self.last_error.lock() {
            *guard = Some(err);
        }
    }

    fn clear_error(&self) {
        if let Ok(mut guard) = self.last_error.lock() {
            *guard = None;
        }
    }

    fn with_conn<F, R>(&self, f: F) -> Result<R, String>
    where
        F: FnOnce(&Connection) -> Result<R, String>,
    {
        let guard = self.conn.lock().map_err(|e| e.to_string())?;
        match guard.as_ref() {
            Some(conn) => f(conn),
            None => Err("Database connection is closed".to_string()),
        }
    }

    /// Return the last error message, or empty string if no error.
    pub fn error(&self) -> String {
        self.last_error
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .unwrap_or_default()
    }

    /// Check if the last operation produced an error.
    pub fn has_error(&self) -> bool {
        self.last_error
            .lock()
            .ok()
            .map(|g| g.is_some())
            .unwrap_or(false)
    }

    /// Execute a SQL statement without parameters, returning modified rows count (-1 on error).
    pub fn execute(&self, sql: String) -> i64 {
        self.clear_error();
        match self.with_conn(|conn| {
            let changes = conn.execute(&sql, []).map_err(|e| e.to_string())?;
            Ok(changes as i64)
        }) {
            Ok(c) => c,
            Err(e) => {
                self.set_error(e);
                -1
            }
        }
    }

    /// Execute a SQL statement with JSON parameters, returning modified rows count (-1 on error).
    pub fn execute_params(&self, sql: String, params_json: String) -> i64 {
        self.clear_error();
        match self.with_conn(|conn| {
            let json_val = parse_json_params(&params_json)?;
            let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
            bind_and_execute(&mut stmt, &json_val)
        }) {
            Ok(c) => c,
            Err(e) => {
                self.set_error(e);
                -1
            }
        }
    }

    /// Execute multiple semicolon-separated statements (DDL, migration scripts).
    pub fn execute_batch(&self, sql: String) -> bool {
        self.clear_error();
        match self.with_conn(|conn| {
            conn.execute_batch(&sql).map_err(|e| e.to_string())?;
            Ok(true)
        }) {
            Ok(b) => b,
            Err(e) => {
                self.set_error(e);
                false
            }
        }
    }

    /// Run a query and return results as a JSON array string.
    pub fn query(&self, sql: String) -> String {
        self.query_params(sql, "[]".to_string())
    }

    /// Run a query with JSON parameters and return results as a JSON array string.
    pub fn query_params(&self, sql: String, params_json: String) -> String {
        self.clear_error();
        match self.with_conn(|conn| {
            let json_val = parse_json_params(&params_json)?;
            let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
            let col_names: Vec<String> = stmt
                .column_names()
                .into_iter()
                .map(|s| s.to_string())
                .collect();

            let rows_json = execute_query_to_json(&mut stmt, &json_val, &col_names)?;
            serde_json::to_string(&rows_json).map_err(|e| e.to_string())
        }) {
            Ok(s) => s,
            Err(e) => {
                self.set_error(e);
                "[]".to_string()
            }
        }
    }

    /// Run a query and return the first row as a JSON object string, or empty string if not found.
    pub fn query_row(&self, sql: String, params_json: String) -> String {
        let all_rows = self.query_params(sql, params_json);
        if let Ok(parsed) = serde_json::from_str::<JsonValue>(&all_rows) {
            if let JsonValue::Array(arr) = parsed {
                if let Some(first) = arr.into_iter().next() {
                    return serde_json::to_string(&first).unwrap_or_default();
                }
            }
        }
        "".to_string()
    }

    /// Return the first column of the first result row as a string.
    pub fn query_string(&self, sql: String, params_json: String) -> String {
        self.clear_error();
        match self.with_conn(|conn| {
            let params = parse_json_params(&params_json)?;
            let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
            let value = match &params {
                JsonValue::Array(values) => {
                    let sqlite_values: Vec<rusqlite::types::Value> =
                        values.iter().map(json_to_sqlite_value).collect();
                    let mut rows = stmt
                        .query(rusqlite::params_from_iter(sqlite_values.iter()))
                        .map_err(|e| e.to_string())?;
                    first_column_as_string(&mut rows)?
                }
                JsonValue::Object(values) => {
                    let named_values: Vec<(String, rusqlite::types::Value)> = values
                        .iter()
                        .map(|(key, value)| {
                            let name = if key.starts_with(':')
                                || key.starts_with('@')
                                || key.starts_with('$')
                            {
                                key.clone()
                            } else {
                                format!(":{}", key)
                            };
                            (name, json_to_sqlite_value(value))
                        })
                        .collect();
                    let refs: Vec<(&str, &dyn rusqlite::ToSql)> = named_values
                        .iter()
                        .map(|(key, value)| (key.as_str(), value as &dyn rusqlite::ToSql))
                        .collect();
                    let mut rows = stmt.query(refs.as_slice()).map_err(|e| e.to_string())?;
                    first_column_as_string(&mut rows)?
                }
                _ => return Err("Parameters must be a JSON array or object".to_string()),
            };
            Ok(value.unwrap_or_default())
        }) {
            Ok(value) => value,
            Err(error) => {
                self.set_error(error);
                String::new()
            }
        }
    }

    /// Prepare a SQL statement and return a `SqliteStatement` struct instance.
    pub fn prepare(&self, sql: String) -> SqliteStatement {
        self.clear_error();
        let valid = self.with_conn(|conn| {
            conn.prepare(&sql).map_err(|e| e.to_string())?;
            Ok(())
        });
        if let Err(e) = valid {
            self.set_error(e.clone());
            SqliteStatement::new_error(sql, e)
        } else {
            let id = self.next_stmt_id.fetch_add(1, Ordering::SeqCst);
            SqliteStatement::new(id, sql, self.conn.clone())
        }
    }

    /// Begin a new transaction and return a `SqliteTransaction` struct instance.
    pub fn transaction(&self) -> SqliteTransaction {
        self.begin();
        SqliteTransaction::new(self.conn.clone(), self.last_error.clone())
    }

    /// Returns the rowid of the most recent successful INSERT.
    pub fn last_insert_rowid(&self) -> i64 {
        self.with_conn(|conn| Ok(conn.last_insert_rowid())).unwrap_or(0)
    }

    /// Returns the number of database rows changed by the most recent statement.
    pub fn changes(&self) -> i64 {
        self.with_conn(|conn| Ok(conn.changes() as i64)).unwrap_or(0)
    }

    /// Returns total row changes since connection open.
    pub fn total_changes(&self) -> i64 {
        self.with_conn(|conn| Ok(conn.total_changes() as i64)).unwrap_or(0)
    }

    /// Returns true if autocommit mode is enabled.
    pub fn is_autocommit(&self) -> bool {
        self.with_conn(|conn| Ok(conn.is_autocommit())).unwrap_or(false)
    }

    /// Returns true if connection is currently open.
    pub fn is_open(&self) -> bool {
        let guard = self.conn.lock();
        match guard {
            Ok(g) => g.is_some(),
            Err(_) => false,
        }
    }

    /// Return the number of columns returned by a prepared SQL statement.
    pub fn stmt_column_count(&self, sql: String) -> i64 {
        self.with_conn(|conn| {
            let stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
            Ok(stmt.column_count() as i64)
        })
        .unwrap_or(0)
    }

    /// Return column names for a SQL statement as a JSON array of strings.
    pub fn stmt_column_names(&self, sql: String) -> String {
        self.with_conn(|conn| {
            let stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
            let names: Vec<String> = stmt
                .column_names()
                .into_iter()
                .map(|s| s.to_string())
                .collect();
            serde_json::to_string(&names).map_err(|e| e.to_string())
        })
        .unwrap_or_else(|_| "[]".to_string())
    }

    /// Close the connection.
    pub fn close(&mut self) -> bool {
        let mut guard = match self.conn.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        if let Some(c) = guard.take() {
            c.close().is_ok()
        } else {
            true
        }
    }

    /// Interrupt any ongoing query.
    pub fn interrupt(&self) -> bool {
        let guard = match self.conn.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        if let Some(ref conn) = *guard {
            let handle = conn.get_interrupt_handle();
            handle.interrupt();
            true
        } else {
            false
        }
    }

    /// Set a busy timeout in milliseconds.
    pub fn set_busy_timeout(&self, ms: i64) -> bool {
        self.with_conn(|conn| {
            conn.busy_timeout(std::time::Duration::from_millis(ms.max(0) as u64))
                .map_err(|e| e.to_string())?;
            Ok(true)
        })
        .unwrap_or(false)
    }

    /// Rebuild and compact the database file.
    pub fn vacuum(&self) -> bool {
        self.execute("VACUUM;".to_string()) >= 0
    }

    /// Backup the database to another file path.
    pub fn backup_to(&self, target_path: String) -> bool {
        self.clear_error();
        match self.with_conn(|src_conn| {
            let mut dest_conn = Connection::open(&target_path).map_err(|e| e.to_string())?;
            let backup = rusqlite::backup::Backup::new(src_conn, &mut dest_conn)
                .map_err(|e| e.to_string())?;
            backup
                .run_to_completion(5, std::time::Duration::from_millis(250), None)
                .map_err(|e| e.to_string())?;
            Ok(true)
        }) {
            Ok(b) => b,
            Err(e) => {
                self.set_error(e);
                false
            }
        }
    }

    // --- Transactions ---

    /// Begin a standard transaction.
    pub fn begin(&self) -> bool {
        self.execute("BEGIN TRANSACTION;".to_string()) >= 0
    }

    /// Begin an immediate transaction.
    pub fn begin_immediate(&self) -> bool {
        self.execute("BEGIN IMMEDIATE TRANSACTION;".to_string()) >= 0
    }

    /// Begin an exclusive transaction.
    pub fn begin_exclusive(&self) -> bool {
        self.execute("BEGIN EXCLUSIVE TRANSACTION;".to_string()) >= 0
    }

    /// Commit the current transaction.
    pub fn commit(&self) -> bool {
        self.execute("COMMIT;".to_string()) >= 0
    }

    /// Roll back the current transaction.
    pub fn rollback(&self) -> bool {
        self.execute("ROLLBACK;".to_string()) >= 0
    }

    /// Create a named savepoint.
    pub fn savepoint(&self, name: String) -> bool {
        let sanitized = sanitize_identifier(&name);
        self.execute(format!("SAVEPOINT {};", sanitized)) >= 0
    }

    /// Roll back to a named savepoint.
    pub fn rollback_to(&self, name: String) -> bool {
        let sanitized = sanitize_identifier(&name);
        self.execute(format!("ROLLBACK TO SAVEPOINT {};", sanitized)) >= 0
    }

    /// Release a named savepoint.
    pub fn release_savepoint(&self, name: String) -> bool {
        let sanitized = sanitize_identifier(&name);
        self.execute(format!("RELEASE SAVEPOINT {};", sanitized)) >= 0
    }

    // --- Schema & Introspection ---

    /// Check if a table exists.
    pub fn table_exists(&self, table_name: String) -> bool {
        let sql = "SELECT COUNT(*) as count FROM sqlite_master WHERE type='table' AND name=?1;";
        let params = json!([table_name]).to_string();
        let row_json = self.query_row(sql.to_string(), params);
        if row_json.is_empty() {
            return false;
        }
        if let Ok(parsed) = serde_json::from_str::<JsonValue>(&row_json) {
            let count = parsed.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
            return count > 0;
        }
        false
    }

    /// Return all user table names as a JSON array of strings.
    pub fn table_names(&self) -> String {
        let sql = "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name;";
        let rows_json = self.query(sql.to_string());
        if let Ok(parsed) = serde_json::from_str::<JsonValue>(&rows_json) {
            let mut names = Vec::new();
            if let JsonValue::Array(arr) = parsed {
                for item in arr {
                    if let Some(name) = item.get("name").and_then(|v| v.as_str()) {
                        names.push(name.to_string());
                    }
                }
            }
            return serde_json::to_string(&names).unwrap_or_else(|_| "[]".to_string());
        }
        "[]".to_string()
    }

    /// Return column metadata for a given table (PRAGMA table_info).
    pub fn table_info(&self, table_name: String) -> String {
        let sanitized = sanitize_identifier(&table_name);
        let sql = format!("PRAGMA table_info({});", sanitized);
        self.query(sql)
    }
}

// ============================================================================
// SqliteStatement
// ============================================================================

/// Native SQLite Prepared Statement struct for Flame
#[derive(Clone)]
pub struct SqliteStatement {
    id: i64,
    sql: String,
    conn: Arc<Mutex<Option<Connection>>>,
    error_msg: Option<String>,
}

impl SqliteStatement {
    fn new(id: i64, sql: String, conn: Arc<Mutex<Option<Connection>>>) -> Self {
        Self {
            id,
            sql,
            conn,
            error_msg: None,
        }
    }

    fn new_error(sql: String, err: String) -> Self {
        Self {
            id: 0,
            sql,
            conn: Arc::new(Mutex::new(None)),
            error_msg: Some(err),
        }
    }

    /// Return statement handle ID.
    pub fn id(&self) -> i64 {
        self.id
    }

    /// Return the SQL string for this statement.
    pub fn sql(&self) -> String {
        self.sql.clone()
    }

    /// Return statement error message if any.
    pub fn error(&self) -> String {
        self.error_msg.clone().unwrap_or_default()
    }

    /// Execute the prepared statement with JSON parameters, returning modified rows count.
    pub fn execute(&self, params_json: String) -> i64 {
        let guard = match self.conn.lock() {
            Ok(g) => g,
            Err(_) => return -1,
        };
        let conn = match guard.as_ref() {
            Some(c) => c,
            None => return -1,
        };
        let json_val = match parse_json_params(&params_json) {
            Ok(v) => v,
            Err(_) => return -1,
        };
        let mut stmt = match conn.prepare(&self.sql) {
            Ok(s) => s,
            Err(_) => return -1,
        };
        bind_and_execute(&mut stmt, &json_val).unwrap_or(-1)
    }

    /// Query the prepared statement with JSON parameters, returning JSON array of rows.
    pub fn query(&self, params_json: String) -> String {
        eprintln!("[debug rust] entered SqliteStatement::query with params: {}", params_json);
        let guard = match self.conn.lock() {
            Ok(g) => g,
            Err(_) => return "[]".to_string(),
        };
        eprintln!("[debug rust] got conn guard in SqliteStatement::query");
        let conn = match guard.as_ref() {
            Some(c) => c,
            None => return "[]".to_string(),
        };
        let json_val = match parse_json_params(&params_json) {
            Ok(v) => v,
            Err(_) => return "[]".to_string(),
        };
        let mut stmt = match conn.prepare(&self.sql) {
            Ok(s) => s,
            Err(_) => return "[]".to_string(),
        };
        let col_names: Vec<String> = stmt
            .column_names()
            .into_iter()
            .map(|s| s.to_string())
            .collect();
        let rows = match execute_query_to_json(&mut stmt, &json_val, &col_names) {
            Ok(r) => r,
            Err(_) => return "[]".to_string(),
        };
        serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string())
    }

    /// Return the number of columns returned by this statement.
    pub fn column_count(&self) -> i64 {
        let guard = match self.conn.lock() {
            Ok(g) => g,
            Err(_) => return 0,
        };
        if let Some(ref conn) = *guard {
            if let Ok(stmt) = conn.prepare(&self.sql) {
                return stmt.column_count() as i64;
            }
        }
        0
    }

    /// Return column names as a JSON array of strings.
    pub fn column_names(&self) -> String {
        let guard = match self.conn.lock() {
            Ok(g) => g,
            Err(_) => return "[]".to_string(),
        };
        if let Some(ref conn) = *guard {
            if let Ok(stmt) = conn.prepare(&self.sql) {
                let names: Vec<String> = stmt
                    .column_names()
                    .into_iter()
                    .map(|s| s.to_string())
                    .collect();
                return serde_json::to_string(&names).unwrap_or_else(|_| "[]".to_string());
            }
        }
        "[]".to_string()
    }
}

// ============================================================================
// SqliteTransaction
// ============================================================================

/// Native SQLite Transaction struct for Flame
#[derive(Clone)]
pub struct SqliteTransaction {
    conn: Arc<Mutex<Option<Connection>>>,
    last_error: Arc<Mutex<Option<String>>>,
    active: Arc<Mutex<bool>>,
}

impl SqliteTransaction {
    fn new(conn: Arc<Mutex<Option<Connection>>>, last_error: Arc<Mutex<Option<String>>>) -> Self {
        Self {
            conn,
            last_error,
            active: Arc::new(Mutex::new(true)),
        }
    }

    /// Returns true if this transaction is still active.
    pub fn is_active(&self) -> bool {
        self.active.lock().map(|g| *g).unwrap_or(false)
    }

    /// Return the last error message from this transaction.
    pub fn error(&self) -> String {
        self.last_error
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .unwrap_or_default()
    }

    /// Execute a SQL statement inside the transaction.
    pub fn execute(&self, sql: String) -> i64 {
        self.execute_params(sql, "[]".to_string())
    }

    /// Execute a SQL statement with JSON parameters inside the transaction.
    pub fn execute_params(&self, sql: String, params_json: String) -> i64 {
        if !self.is_active() {
            return -1;
        }
        let guard = match self.conn.lock() {
            Ok(g) => g,
            Err(_) => return -1,
        };
        let conn = match guard.as_ref() {
            Some(c) => c,
            None => return -1,
        };
        let json_val = match parse_json_params(&params_json) {
            Ok(v) => v,
            Err(_) => return -1,
        };
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return -1,
        };
        bind_and_execute(&mut stmt, &json_val).unwrap_or(-1)
    }

    /// Run a query inside the transaction.
    pub fn query(&self, sql: String) -> String {
        self.query_params(sql, "[]".to_string())
    }

    /// Run a query with JSON parameters inside the transaction.
    pub fn query_params(&self, sql: String, params_json: String) -> String {
        if !self.is_active() {
            return "[]".to_string();
        }
        let guard = match self.conn.lock() {
            Ok(g) => g,
            Err(_) => return "[]".to_string(),
        };
        let conn = match guard.as_ref() {
            Some(c) => c,
            None => return "[]".to_string(),
        };
        let json_val = match parse_json_params(&params_json) {
            Ok(v) => v,
            Err(_) => return "[]".to_string(),
        };
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return "[]".to_string(),
        };
        let col_names: Vec<String> = stmt
            .column_names()
            .into_iter()
            .map(|s| s.to_string())
            .collect();
        let rows = match execute_query_to_json(&mut stmt, &json_val, &col_names) {
            Ok(r) => r,
            Err(_) => return "[]".to_string(),
        };
        serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string())
    }

    /// Commit this transaction.
    pub fn commit(&self) -> bool {
        if !self.is_active() {
            return false;
        }
        let guard = match self.conn.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        if let Some(ref conn) = *guard {
            let res = conn.execute("COMMIT;", []).is_ok();
            if let Ok(mut act) = self.active.lock() {
                *act = false;
            }
            res
        } else {
            false
        }
    }

    /// Roll back this transaction.
    pub fn rollback(&self) -> bool {
        if !self.is_active() {
            return false;
        }
        let guard = match self.conn.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        if let Some(ref conn) = *guard {
            let res = conn.execute("ROLLBACK;", []).is_ok();
            if let Ok(mut act) = self.active.lock() {
                *act = false;
            }
            res
        } else {
            false
        }
    }

    /// Create a named savepoint within this transaction.
    pub fn savepoint(&self, name: String) -> bool {
        let sanitized = sanitize_identifier(&name);
        self.execute(format!("SAVEPOINT {};", sanitized)) >= 0
    }

    /// Roll back to a named savepoint.
    pub fn rollback_to(&self, name: String) -> bool {
        let sanitized = sanitize_identifier(&name);
        self.execute(format!("ROLLBACK TO SAVEPOINT {};", sanitized)) >= 0
    }

    /// Release a named savepoint.
    pub fn release_savepoint(&self, name: String) -> bool {
        let sanitized = sanitize_identifier(&name);
        self.execute(format!("RELEASE SAVEPOINT {};", sanitized)) >= 0
    }
}

// ============================================================================
// Module-level functions
// ============================================================================

/// Open a SQLite database file at the given path.
pub fn open(path: String) -> SqliteConnection {
    match Connection::open(&path) {
        Ok(conn) => SqliteConnection::new(conn),
        Err(e) => SqliteConnection::new_error(e.to_string()),
    }
}

/// Open an in-memory SQLite database.
pub fn open_in_memory() -> SqliteConnection {
    match Connection::open_in_memory() {
        Ok(conn) => SqliteConnection::new(conn),
        Err(e) => SqliteConnection::new_error(e.to_string()),
    }
}

/// Open a SQLite database with specific flags (read-only, create, read-write).
pub fn open_with_flags(path: String, read_only: bool, create: bool) -> SqliteConnection {
    let mut flags = if read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    };
    if create {
        flags |= OpenFlags::SQLITE_OPEN_CREATE;
    }
    match Connection::open_with_flags(&path, flags) {
        Ok(conn) => SqliteConnection::new(conn),
        Err(e) => SqliteConnection::new_error(e.to_string()),
    }
}

/// Return the compiled SQLite library version.
pub fn sqlite_version() -> String {
    rusqlite::version().to_string()
}

// ============================================================================
// Internal Helpers
// ============================================================================

fn sanitize_identifier(ident: &str) -> String {
    ident.replace('"', "").replace(';', "").replace(' ', "")
}

fn parse_json_params(params_json: &str) -> Result<JsonValue, String> {
    let trimmed = params_json.trim();
    if trimmed.is_empty() || trimmed == "[]" {
        return Ok(JsonValue::Array(Vec::new()));
    }
    let mut val: JsonValue = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(_) => {
            return Ok(JsonValue::Array(vec![JsonValue::String(trimmed.to_string())]));
        }
    };
    if let JsonValue::String(ref s) = val {
        let s_trimmed = s.trim();
        if (s_trimmed.starts_with('[') && s_trimmed.ends_with(']'))
            || (s_trimmed.starts_with('{') && s_trimmed.ends_with('}'))
        {
            if let Ok(nested) = serde_json::from_str::<JsonValue>(s_trimmed) {
                val = nested;
            }
        }
    }
    if !val.is_array() && !val.is_object() {
        val = JsonValue::Array(vec![val]);
    }
    Ok(val)
}

fn json_to_sqlite_value(val: &JsonValue) -> rusqlite::types::Value {
    match val {
        JsonValue::Null => rusqlite::types::Value::Null,
        JsonValue::Bool(b) => rusqlite::types::Value::Integer(if *b { 1 } else { 0 }),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                rusqlite::types::Value::Integer(i)
            } else if let Some(f) = n.as_f64() {
                rusqlite::types::Value::Real(f)
            } else {
                rusqlite::types::Value::Text(n.to_string())
            }
        }
        JsonValue::String(s) => rusqlite::types::Value::Text(s.clone()),
        JsonValue::Array(_) | JsonValue::Object(_) => rusqlite::types::Value::Text(val.to_string()),
    }
}

fn bind_and_execute(stmt: &mut rusqlite::Statement, params: &JsonValue) -> Result<i64, String> {
    match params {
        JsonValue::Array(arr) => {
            let sqlite_vals: Vec<rusqlite::types::Value> =
                arr.iter().map(json_to_sqlite_value).collect();
            let count = stmt
                .execute(rusqlite::params_from_iter(sqlite_vals.iter()))
                .map_err(|e| e.to_string())?;
            Ok(count as i64)
        }
        JsonValue::Object(map) => {
            let mut named_params = Vec::new();
            for (k, v) in map {
                let name = if k.starts_with(':') || k.starts_with('@') || k.starts_with('$') {
                    k.clone()
                } else {
                    format!(":{}", k)
                };
                named_params.push((name, json_to_sqlite_value(v)));
            }
            let refs: Vec<(&str, &dyn rusqlite::ToSql)> = named_params
                .iter()
                .map(|(k, v)| (k.as_str(), v as &dyn rusqlite::ToSql))
                .collect();
            let count = stmt
                .execute(refs.as_slice())
                .map_err(|e| e.to_string())?;
            Ok(count as i64)
        }
        _ => Err("Parameters must be a JSON array or object".to_string()),
    }
}

fn execute_query_to_json(
    stmt: &mut rusqlite::Statement,
    params: &JsonValue,
    col_names: &[String],
) -> Result<Vec<JsonValue>, String> {
    let mut rows_out = Vec::new();

    let mut handle_rows = |mut rows: rusqlite::Rows| -> Result<(), String> {
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let mut map = Map::new();
            for (idx, col_name) in col_names.iter().enumerate() {
                let val_ref = row.get_ref(idx).map_err(|e| e.to_string())?;
                let json_val = match val_ref {
                    rusqlite::types::ValueRef::Null => JsonValue::Null,
                    rusqlite::types::ValueRef::Integer(v) => json!(v),
                    rusqlite::types::ValueRef::Real(v) => json!(v),
                    rusqlite::types::ValueRef::Text(v) => {
                        let s = std::str::from_utf8(v).unwrap_or("");
                        JsonValue::String(s.to_string())
                    }
                    rusqlite::types::ValueRef::Blob(v) => {
                        let bytes: Vec<JsonValue> = v.iter().map(|&b| json!(b)).collect();
                        JsonValue::Array(bytes)
                    }
                };
                map.insert(col_name.clone(), json_val);
            }
            rows_out.push(JsonValue::Object(map));
        }
        Ok(())
    };

    match params {
        JsonValue::Array(arr) => {
            let sqlite_vals: Vec<rusqlite::types::Value> =
                arr.iter().map(json_to_sqlite_value).collect();
            let rows = stmt
                .query(rusqlite::params_from_iter(sqlite_vals.iter()))
                .map_err(|e| e.to_string())?;
            handle_rows(rows)?;
        }
        JsonValue::Object(map) => {
            let mut named_params = Vec::new();
            for (k, v) in map {
                let name = if k.starts_with(':') || k.starts_with('@') || k.starts_with('$') {
                    k.clone()
                } else {
                    format!(":{}", k)
                };
                named_params.push((name, json_to_sqlite_value(v)));
            }
            let refs: Vec<(&str, &dyn rusqlite::ToSql)> = named_params
                .iter()
                .map(|(k, v)| (k.as_str(), v as &dyn rusqlite::ToSql))
                .collect();
            let rows = stmt
                .query(refs.as_slice())
                .map_err(|e| e.to_string())?;
            handle_rows(rows)?;
        }
        _ => return Err("Parameters must be a JSON array or object".to_string()),
    }

    Ok(rows_out)
}

fn first_column_as_string(rows: &mut rusqlite::Rows) -> Result<Option<String>, String> {
    let Some(row) = rows.next().map_err(|error| error.to_string())? else {
        return Ok(None);
    };

    let value = match row.get_ref(0).map_err(|error| error.to_string())? {
        rusqlite::types::ValueRef::Null => String::new(),
        rusqlite::types::ValueRef::Integer(value) => value.to_string(),
        rusqlite::types::ValueRef::Real(value) => value.to_string(),
        rusqlite::types::ValueRef::Text(value) => std::str::from_utf8(value)
            .map_err(|error| error.to_string())?
            .to_string(),
        rusqlite::types::ValueRef::Blob(value) => String::from_utf8_lossy(value).into_owned(),
    };
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_and_disk_db() {
        let db = open_in_memory();
        assert!(db.is_open());
        assert!(db.is_autocommit());

        db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, score REAL);".to_string());
        let res = db.execute_params("INSERT INTO users (name, score) VALUES (?1, ?2);".to_string(), "[\"Alice\", 98.5]".to_string());
        assert_eq!(res, 1);
        assert_eq!(db.last_insert_rowid(), 1);

        let rows = db.query("SELECT * FROM users;".to_string());
        assert!(rows.contains("Alice"));
        assert_eq!(
            db.query_string(
                "SELECT name FROM users WHERE id = ?1;".to_string(),
                "[1]".to_string()
            ),
            "Alice"
        );

        let stmt = db.prepare("SELECT name FROM users WHERE score > ?1;".to_string());
        assert_eq!(stmt.column_count(), 1);
        let names = stmt.column_names();
        assert_eq!(names, "[\"name\"]");

        let stmt_rows = stmt.query("[90.0]".to_string());
        assert!(stmt_rows.contains("Alice"));

        // Test double-encoded JSON strings (as produced by json.stringify on a string)
        let double_encoded = "\"[\"Alice\", 98.5]\"";
        let parsed = parse_json_params(double_encoded).unwrap();
        assert!(parsed.is_array());

        let scalar_parsed = parse_json_params("\"[1]\"").unwrap();
        assert!(scalar_parsed.is_array());

        // Disk DB test
        let disk_db = open("target/test_disk.db".to_string());
        assert!(disk_db.is_open());
        disk_db.execute("CREATE TABLE IF NOT EXISTS kv (k TEXT PRIMARY KEY, v TEXT);".to_string());
        disk_db.execute_params("INSERT OR REPLACE INTO kv (k, v) VALUES (?1, ?2);".to_string(), "[\"testKey\", \"testVal\"]".to_string());
        let disk_rows = disk_db.query("SELECT * FROM kv WHERE k = 'testKey';".to_string());
        assert!(disk_rows.contains("testVal"));

        // Error detection test
        db.execute("INSERT INTO non_existent_table VALUES (1);".to_string());
        assert!(db.has_error());
        assert!(db.error().contains("no such table"));
    }
}
