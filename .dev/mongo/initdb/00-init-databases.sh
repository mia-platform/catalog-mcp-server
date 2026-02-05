#!/usr/bin/env bash
set -euo pipefail

INIT_DIR="/docker-entrypoint-initdb.d"

# Define the databases to initialize
DATABASES=("system" "org_1")

echo "Starting MongoDB initialization..."

for DB_NAME in "${DATABASES[@]}"; do
    echo "Setting up database: $DB_NAME"
    
    # Run the setup script directly with the database name
    mongosh --quiet --norc --eval "
        const dbName = '$DB_NAME';
        const db = db.getSiblingDB(dbName);
        
        // Load and execute the setup module
        load('$INIT_DIR/setup-module.js');
        
        // Apply the schema
        print('Applying schema contract to db: ' + dbName);
        applySchemaContract(db);
        print('Done.');
    "
    
    echo "✓ Database $DB_NAME initialized successfully"
done

echo "MongoDB initialization complete!"
