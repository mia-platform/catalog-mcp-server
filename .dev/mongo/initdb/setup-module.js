const CONTRACT = {
  collections: [
    {
      name: 'item_type_definitions',
      indexes: [
        { name: 'pk', key: { 'apiVersion': 1, 'kind': 1, 'metadata.name': 1 }, unique: true },
        { name: 'metadata.creationTimestamp', key: { 'metadata.creationTimestamp': 1 } },
        { name: 'metadata.labels', key: { 'metadata.labels.k': 1, 'metadata.labels.v': 1 } },
        { name: 'metadata.name', key: { 'metadata.name': 1 } },
        { name: 'metadata.namespace', key: { 'metadata.namespace': 1 } },
        { name: 'metadata.title', key: { 'metadata.title': 1 } },
        { name: 'metadata.tags', key: { 'metadata.tags': 1 } },
        { name: 'spec.scope', key: { 'spec.scope': 1 } },
        { name: 'spec.group', key: { 'spec.group': 1 } },
        { name: 'spec.names.kind', key: { 'spec.names.kind': 1 } },
        { name: 'spec.names.plural', key: { 'spec.names.plural': 1 }  }
      ]
    },
    {
      name: 'items',
      indexes: [
        { name: 'pk', key: { 'data.apiVersion': 1, 'data.kind': 1, 'data.metadata.name': 1 }, unique: true },
        { name: 'idx', key: { 'idx.k': 1, 'idx.v': 1, 'idx.t': 1 } },
        { name: 'data.apiVersion', key: { 'data.apiVersion': 1 } },
        { name: 'data.kind', key: { 'data.kind': 1 } },
        { name: 'data.metadata.creationTimestamp', key: { 'data.metadata.creationTimestamp': 1 } },
        { name: 'data.metadata.labels', key: { 'data.metadata.labels.k': 1, 'data.metadata.labels.v': 1 } },
        { name: 'data.metadata.name', key: { 'data.metadata.name': 1 } },
        { name: 'data.metadata.namespace', key: { 'data.metadata.namespace': 1 } },
        { name: 'data.metadata.title', key: { 'data.metadata.title': 1 } },
        { name: 'data.metadata.tags', key: { 'data.metadata.tags': 1 } }
      ]
    }
  ]
}

const compactObject = (obj) => {
  const out = {}

  Object.keys(obj).forEach(k => {
    if (obj[k] !== undefined) out[k] = obj[k]
  })

  return out
}

const ensureCollection = (database, spec) => {
  const infos = database.getCollectionInfos({ name: spec.name })

  if (infos.length === 0) {
    database.createCollection(spec.name, spec.options || {})
    print(`Created collection: ${spec.name}`)
  }
}

const ensureIndexes = (database, spec) => {
  const coll = database.getCollection(spec.name)
  const existingIndexNames = {}

  for (let i = 0; i < coll.getIndexes().length; i++) {
    existingIndexNames[coll.getIndexes()[i].name] = true
  }

  (spec.indexes || []).forEach(idx => {
    if (!idx || !idx.name || !idx.key) {
      throw new Error(`Invalid index spec for collection '${spec.name}': ${tojson(idx)}`)
    }

    if (existingIndexNames[idx.name]) { return }

    const options = compactObject({
      name: idx.name,
      unique: idx.unique,
      sparse: idx.sparse,
      expireAfterSeconds: idx.expireAfterSeconds,
      partialFilterExpression: idx.partialFilterExpression,
      collation: idx.collation,
      background: idx.background
    })

    coll.createIndex(idx.key, options)

    print(`Created index: ${spec.name}.${idx.name}`)
  })
}

const applySchemaContract = (database, dirname) => {
  if (!CONTRACT || !Array.isArray(CONTRACT.collections)) {
    throw new Error('Schema contract must be a JSON object with a "collections" array')
  }

  (CONTRACT.collections || []).forEach(spec => {
    if (!spec || !spec.name) { throw new Error(`Invalid collection spec: ${tojson(spec)}`) }

    ensureCollection(database, spec)
    ensureIndexes(database, spec)
  })
}
