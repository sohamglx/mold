# mold

SQLite package for Flame.

## Installation

Add the package and compile it:

```bash
fmp add https://github.com/sohamglx/mold
fmp install
```

---

## Quick Start

```flame
import mold

let db = mold.open("app.db")

// Create a table
db.execute("CREATE TABLE IF NOT EXISTS users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)")

// Insert data with parameters
db.executeParams("INSERT INTO users (name, age) VALUES (?1, ?2)", '["Alice", 30]')

// Query rows as JSON
let rows = db.query("SELECT * FROM users")
println(rows)

db.close()
```

---

## Opening Databases

```flame
// File database (creates if doesn't exist)
let db = mold.open("app.db")

// In-memory database
let mem = mold.openInMemory()

// Open with custom flags (path, readOnly, create)
let readOnlyDb = mold.openWithFlags("app.db", true, false)
```

---

## Executing SQL & Queries

### Execute (INSERT, UPDATE, DELETE)

```flame
// Without parameters
let changes = db.execute("DELETE FROM users WHERE age < 18")

// With positional parameters (JSON array string)
db.executeParams("INSERT INTO users (name, age) VALUES (?1, ?2)", '["Bob", 25]')

// With named parameters (JSON object string)
db.executeParams("INSERT INTO users (name, age) VALUES (:name, :age)", '{"name": "Charlie", "age": 28}')

// Execute multiple statements (schema setup, migrations)
db.executeBatch("CREATE TABLE a (id INT); CREATE TABLE b (id INT);")
```

### Query (SELECT)

All query results are returned as JSON strings.

```flame
// Query all rows
let allRows = db.query("SELECT * FROM users")
// Output: [{"age":30,"id":1,"name":"Alice"},{"age":25,"id":2,"name":"Bob"}]

// Query with parameters
let adultUsers = db.queryParams("SELECT * FROM users WHERE age >= ?1", '[21]')

// Query a single row
let user = db.queryRow("SELECT * FROM users WHERE id = ?1", '[1]')
// Output: {"age":30,"id":1,"name":"Alice"}
```

---

## Prepared Statements

Reuse compiled statements for queries and schema inspection:

```flame
let stmt = db.prepare("SELECT name FROM users WHERE age > ?1")

// Query rows
let rows = stmt.query('[20]')

// Inspect statement schema
let count = stmt.columnCount()      // Number of result columns
let names = stmt.columnNames()      // Column names: ["name"]
let sql = stmt.sql()                // Original SQL query text

// Execute update with statement
let insertStmt = db.prepare("INSERT INTO users (name, age) VALUES (?1, ?2)")
insertStmt.execute('["David", 35]')
```

---

## Transactions & Savepoints

### Managed Transactions

```flame
let tx = db.transaction()

tx.execute("INSERT INTO users (name, age) VALUES ('Eve', 22)")

// Nested savepoint
tx.savepoint("step1")
tx.execute("INSERT INTO users (name, age) VALUES ('Frank', 19)")
tx.rollbackTo("step1") // rolls back Frank, keeps Eve

tx.commit()
```

### Manual Transactions

```flame
db.begin()
// or db.beginImmediate()
// or db.beginExclusive()

db.execute("UPDATE accounts SET balance = balance - 100 WHERE id = 1")
db.execute("UPDATE accounts SET balance = balance + 100 WHERE id = 2")

db.commit()
// or db.rollback()
```

---

## Utilities & Diagnostics

```flame
// Last inserted ID and changes count
let id = db.lastInsertRowid()
let changes = db.changes()
let total = db.totalChanges()

// Table introspection
let exists = db.tableExists("users")
let tables = db.tableNames()         // JSON array of table names
let info = db.tableInfo("users")     // PRAGMA table_info metadata

// Maintenance & Configuration
db.setBusyTimeout(5000)              // Wait up to 5000ms if database is locked
db.vacuum()                          // Compact and optimize database file
db.backupTo("backup.db")             // Online zero-downtime database backup

// Error handling
if db.hasError() {
    println("SQLite Error: " + db.error())
}

// Close connection
db.close()

// Check SQLite version
let version = mold.sqliteVersion()
println("SQLite version: " + version)
```

