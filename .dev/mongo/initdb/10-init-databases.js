const fs = require('node:fs')

const CRDS_COLLECTION_NAME = 'custom_resource_definitions'
const OBJECTS_COLLECTION_NAME = 'objects'

const createCRDsCollection = (db) => {
  db.createCollection(CRDS_COLLECTION_NAME)

  db[CRDS_COLLECTION_NAME].createIndex(
    { apiVersion: 1, kind: 1, 'metadata.name': 1 },
    { name: 'pk', unique: true }
  )

  db[CRDS_COLLECTION_NAME].createIndex({ 'metadata.labels': 1 })
  db[CRDS_COLLECTION_NAME].createIndex({ 'metadata.name': 1 })
  db[CRDS_COLLECTION_NAME].createIndex({ 'metadata.namespace': 1 })
  db[CRDS_COLLECTION_NAME].createIndex({ 'metadata.title': 1 })
  db[CRDS_COLLECTION_NAME].createIndex({ 'metadata.tags': 1 })
  db[CRDS_COLLECTION_NAME].createIndex({ 'spec.scope': 1 })
  db[CRDS_COLLECTION_NAME].createIndex({ 'spec.group': 1 })
  db[CRDS_COLLECTION_NAME].createIndex({ 'spec.names.kind': 1 })
  db[CRDS_COLLECTION_NAME].createIndex({ 'spec.names.plural': 1 })
}

const createObjectsCollections = (db) => {
  db.createCollection(OBJECTS_COLLECTION_NAME)

  db[OBJECTS_COLLECTION_NAME].createIndex(
    { 'data.apiVersion': 1, 'data.kind': 1, 'data.metadata.name': 1 },
    { name: 'pk', unique: true }
  )

  db[OBJECTS_COLLECTION_NAME].createIndex({ 'data.metadata.labels': 1 })
  db[OBJECTS_COLLECTION_NAME].createIndex({ 'data.metadata.name': 1 })
  db[OBJECTS_COLLECTION_NAME].createIndex({ 'data.metadata.namespace': 1 })
  db[OBJECTS_COLLECTION_NAME].createIndex({ 'data.metadata.title': 1 })
  db[OBJECTS_COLLECTION_NAME].createIndex({ 'data.metadata.tags': 1 })
  db[OBJECTS_COLLECTION_NAME].createIndex({ 'indexedFields.f0': 1 })
  db[OBJECTS_COLLECTION_NAME].createIndex({ 'indexedFields.f1': 1 })
  db[OBJECTS_COLLECTION_NAME].createIndex({ 'indexedFields.f2': 1 })
  db[OBJECTS_COLLECTION_NAME].createIndex({ 'indexedFields.f3': 1 })
  db[OBJECTS_COLLECTION_NAME].createIndex({ 'indexedFields.f4': 1 })
  db[OBJECTS_COLLECTION_NAME].createIndex({ 'indexedFields.f5': 1 })
  db[OBJECTS_COLLECTION_NAME].createIndex({ 'indexedFields.f6': 1 })
  db[OBJECTS_COLLECTION_NAME].createIndex({ 'indexedFields.f7': 1 })
}

const orgs = fs.readdirSync(path.join(__dirname, 'data'), { withFileTypes: true })
  .filter(dirent => dirent.isDirectory())
  .map(dirent => dirent.name)

for (const org of orgs) {
  db = db.getSiblingDB(org)
  createCRDsCollection(db)
  createObjectsCollections(db)
}
