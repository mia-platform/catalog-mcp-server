const seedItemTypeDefinition = (dbName, manifestFilename) => {
  db = db.getSiblingDB(dbName)

  const manifestFilePath = path.join(__dirname, 'data', 'item-type-definitions', manifestFilename)
  const manifest = JSON.parse(fs.readFileSync(manifestFilePath, 'utf-8'))

  delete manifest.$schema

  manifest.metadata.creationTimestamp = new Date().toISOString()
  manifest.metadata.uid = crypto.randomUUID()
  manifest._v = 0

  db['item_type_definitions'].insertOne(manifest)
}

const seedItem = (dbName, manifestFilename) => {
  db = db.getSiblingDB(dbName)

  const manifestFilePath = path.join(__dirname, 'data', 'items', manifestFilename)
  const manifest = JSON.parse(fs.readFileSync(manifestFilePath, 'utf-8'))

  delete manifest.$schema

  manifest.data.metadata.creationTimestamp = new Date().toISOString()
  manifest.data.metadata.uid = crypto.randomUUID()
  manifest.__v = 0

  db['items'].insertOne(manifest)
}

seedItemTypeDefinition('system', 'mia-platform.eu.services.json')
seedItemTypeDefinition('system', 'mia-platform.eu.templates.json')
seedItemTypeDefinition('org_1', 'org-1.com.dockerimages.json')

seedItem('org_1', 'service.api-gateway.json')
seedItem('org_1', 'service.crud-service.json')
