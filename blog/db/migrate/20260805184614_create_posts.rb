class CreatePosts < ActiveRecord::Migration[8.1]
  def change
    create_table :posts do |t|
      t.string   :title,   null: false
      t.string   :slug,    null: false
      t.text     :body,    null: false, default: ""
      t.text     :summary, null: false, default: ""

      # NULL means draft. A nullable timestamp carries both facts — whether the
      # post is public and when it went public — in one column, so there is no
      # way to be published without a date, or dated without being published.
      t.datetime :published_at

      t.timestamps
    end

    # The slug is the public URL, so uniqueness is enforced by the database and
    # not only by the model: a validation alone loses the race between two
    # concurrent creates.
    add_index :posts, :slug, unique: true

    # The public index is "published posts, newest first" — the one query this
    # app runs on nearly every request.
    add_index :posts, :published_at
  end
end
