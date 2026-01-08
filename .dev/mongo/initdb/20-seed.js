const crypto = require('node:crypto')
const fs = require('node:fs')

const CRDS_COLLECTION_NAME = 'custom_resource_definitions'
const OBJECTS_COLLECTION_NAME = 'objects'

const seedCRDs = (db, orgPath) => {
  const seed = JSON.parse(fs.readFileSync(`${orgPath}/crds.mock.json`, 'utf-8'))
  seed.forEach(crd => {
    crd.__v = 0
    crd.metadata.creationTimestamp = new Date().toISOString()
    crd.metadata.uid = crypto.randomUUID()

    db[CRDS_COLLECTION_NAME].insertOne(crd)
  })
}

const seedObjects = (db, orgPath) => {
  const seed = JSON.parse(fs.readFileSync(`${orgPath}/objects.mock.json`, 'utf-8'))
    seed.forEach(obj => {
    obj.__v = 0
    obj.data.metadata.creationTimestamp = new Date().toISOString()
    obj.data.metadata.uid = crypto.randomUUID()

    db[OBJECTS_COLLECTION_NAME].insertOne(obj)
  })
}

fs.readdirSync(path.join(__dirname, 'data'), { withFileTypes: true })
  .filter(dirent => dirent.isDirectory())
  .forEach(dirent => {
    const org = dirent.name
    const orgPath = path.join(dirent.parentPath, org)

    db = db.getSiblingDB(org)

    seedCRDs(db, orgPath)
    seedObjects(db, orgPath)
  })
